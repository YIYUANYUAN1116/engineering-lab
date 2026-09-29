use std::collections::HashSet;
use std::env;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::fs::FileExt;
use std::sync::Arc;
use std::time::Instant;

use reqwest::header::{CONTENT_LENGTH, RANGE};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, watch};
use tokio::time::{self, Duration};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() != 2 {
        println!("usage: rget <url>");
        return Ok(());
    }

    let url = &args[1];

    let output_path = "download.bin";
    let temp_path = "download.bin.part";
    let state_path = "download.bin.state";

    let client = reqwest::Client::new();

    // ---------------------------------------------------------
    // 1. HEAD 获取文件大小
    // ---------------------------------------------------------

    let head_response = client.head(url).send().await?;

    println!("HEAD status: {}", head_response.status());

    let total_size = head_response
        .headers()
        .get(CONTENT_LENGTH)
        .ok_or_else(|| anyhow::anyhow!("missing Content-Length"))?
        .to_str()?
        .parse::<u64>()?;

    println!("total size: {total_size} bytes");

    // ---------------------------------------------------------
    // 2. Range 配置
    // ---------------------------------------------------------

    let concurrency: u64 = 4;
    let range_size = total_size.div_ceil(concurrency);

    // ---------------------------------------------------------
    // 3. 加载之前已经完成的 Range
    // ---------------------------------------------------------

    let completed = load_completed_ranges(state_path).await?;

    println!("completed ranges: {:?}", completed);

    // 计算恢复时已经完成了多少字节
    let resumed_bytes =
        calculate_completed_bytes(&completed, total_size, range_size);

    if resumed_bytes > 0 {
        println!(
            "resume from {:.2} MiB",
            resumed_bytes as f64 / 1024.0 / 1024.0
        );
    }

    // ---------------------------------------------------------
    // 4. 打开 .part 文件
    //
    // 注意：不能 File::create()，否则会 truncate，
    // 上一次已经下载的数据会丢失。
    // ---------------------------------------------------------

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(temp_path)?;

    file.set_len(total_size)?;

    let file = Arc::new(file);

    // ---------------------------------------------------------
    // 5. progress channel
    //
    // worker 每写完一个 chunk：
    //
    // tx.send(chunk_len)
    //
    // progress task 负责累计。
    // ---------------------------------------------------------

    let (tx, mut rx) = mpsc::channel::<u64>(100);

    // ---------------------------------------------------------
    // 6. Range 完成 channel
    //
    // 一个 Range 完整完成后：
    //
    // done_tx.send(index)
    //
    // state task 写入 .state
    // ---------------------------------------------------------

    let (done_tx, mut done_rx) = mpsc::channel::<u64>(16);

    // ---------------------------------------------------------
    // 7. cancellation channel
    // ---------------------------------------------------------

    let (cancel_tx, cancel_rx) = watch::channel(false);

    // ---------------------------------------------------------
    // 8. progress task
    // ---------------------------------------------------------

    let progress_handle = tokio::spawn(async move {
        let start_time = Instant::now();

        // 已完成的 Range 也算进当前下载进度
        let mut downloaded = resumed_bytes;

        let mut last_downloaded = downloaded;
        let mut last_tick = Instant::now();

        let mut interval = time::interval(Duration::from_millis(500));

        loop {
            tokio::select! {
                message = rx.recv() => {
                    match message {
                        Some(bytes) => {
                            downloaded += bytes;
                        }

                        None => {
                            print_progress(
                                downloaded,
                                total_size,
                                start_time,
                                last_downloaded,
                                last_tick,
                            );

                            println!();
                            break;
                        }
                    }
                }

                _ = interval.tick() => {
                    print_progress(
                        downloaded,
                        total_size,
                        start_time,
                        last_downloaded,
                        last_tick,
                    );

                    last_downloaded = downloaded;
                    last_tick = Instant::now();
                }
            }
        }
    });

    // ---------------------------------------------------------
    // 9. state writer task
    // ---------------------------------------------------------

    let state_path_owned = state_path.to_string();

    let state_handle = tokio::spawn(async move {
        let mut state_file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(state_path_owned)
            .await?;

        while let Some(index) = done_rx.recv().await {
            state_file
                .write_all(format!("{index}\n").as_bytes())
                .await?;

            state_file.flush().await?;
        }

        Ok::<(), anyhow::Error>(())
    });

    // ---------------------------------------------------------
    // 10. Ctrl+C task
    // ---------------------------------------------------------

    let ctrl_c_cancel_tx = cancel_tx.clone();

    let ctrl_c_handle = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            println!("\ncancelling...");

            let _ = ctrl_c_cancel_tx.send(true);
        }
    });

    // ---------------------------------------------------------
    // 11. 创建 Range worker
    // ---------------------------------------------------------

    let mut handles = Vec::new();

    for i in 0..concurrency {
        let start = i * range_size;

        if start >= total_size {
            break;
        }

        let end = ((i + 1) * range_size - 1)
            .min(total_size - 1);

        // 已完成 Range 直接跳过
        if completed.contains(&i) {
            println!("range {i}: already completed, skip");
            continue;
        }

        println!("range {i}: {start}-{end}");

        let client = client.clone();
        let url = url.clone();

        let file = Arc::clone(&file);

        let tx = tx.clone();
        let done_tx = done_tx.clone();

        let cancel_rx = cancel_rx.clone();

        let handle = tokio::spawn(async move {
            download_range(
                client,
                url,
                file,
                tx,
                done_tx,
                cancel_rx,
                i,
                start,
                end,
            )
            .await
        });

        handles.push(handle);
    }

    // ---------------------------------------------------------
    // 12. 等待所有 Range
    // ---------------------------------------------------------

    for handle in handles {
        handle.await??;
    }

    // main 自己持有的 Sender 必须释放
    drop(tx);
    drop(done_tx);

    progress_handle.await?;

    state_handle.await??;

    // 已经正常完成，不再需要 Ctrl+C task
    ctrl_c_handle.abort();

    // ---------------------------------------------------------
    // 13. 释放 File
    // ---------------------------------------------------------

    drop(file);

    // ---------------------------------------------------------
    // 14. 全部 Range 成功
    //
    // .part -> 正式文件
    // ---------------------------------------------------------

    tokio::fs::rename(temp_path, output_path).await?;

    // ---------------------------------------------------------
    // 15. 删除状态文件
    // ---------------------------------------------------------

    match tokio::fs::remove_file(state_path).await {
        Ok(_) => {}

        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}

        Err(err) => return Err(err.into()),
    }

    println!("download finished: {output_path}");

    Ok(())
}

// ============================================================================
// 下载一个 Range
// ============================================================================

async fn download_range(
    client: reqwest::Client,
    url: String,
    file: Arc<std::fs::File>,
    tx: mpsc::Sender<u64>,
    done_tx: mpsc::Sender<u64>,
    mut cancel_rx: watch::Receiver<bool>,
    index: u64,
    start: u64,
    end: u64,
) -> anyhow::Result<()> {
    let range = format!("bytes={start}-{end}");

    let mut response = client
        .get(&url)
        .header(RANGE, range)
        .send()
        .await?;

    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        anyhow::bail!(
            "range {index}: server does not support Range, status={}",
            response.status()
        );
    }

    let mut offset = start;
    let mut received: u64 = 0;

    loop {
        // -----------------------------------------------------
        // 同时等待：
        //
        // 1. HTTP chunk
        // 2. cancel
        // -----------------------------------------------------

        let chunk = tokio::select! {
            result = response.chunk() => {
                match result? {
                    Some(chunk) => chunk,
                    None => break,
                }
            }

            result = cancel_rx.changed() => {
                if result.is_err() {
                    anyhow::bail!(
                        "range {index}: cancel channel closed"
                    );
                }

                if *cancel_rx.borrow() {
                    anyhow::bail!(
                        "range {index}: download cancelled"
                    );
                }

                continue;
            }
        };

        let chunk_len = chunk.len() as u64;

        // blocking write 使用自己的 Arc clone
        let file = Arc::clone(&file);

        tokio::task::spawn_blocking(move || {
            write_all_at(
                &file,
                &chunk,
                offset,
            )
        })
        .await??;

        offset += chunk_len;
        received += chunk_len;

        // progress 失败不应该导致下载失败
        let _ = tx.send(chunk_len).await;
    }

    // ---------------------------------------------------------
    // 校验这个 Range 是否完整
    // ---------------------------------------------------------

    let expected = end - start + 1;

    if received != expected {
        anyhow::bail!(
            "range {index}: length mismatch, expected={expected}, actual={received}"
        );
    }

    // ---------------------------------------------------------
    // 真正完整完成后，才写 state
    // ---------------------------------------------------------

    done_tx.send(index).await?;

    Ok(())
}

// ============================================================================
// positional write
//
// write_at 不保证一次把整个 buffer 写完，
// 所以自己实现 write_all_at。
// ============================================================================

fn write_all_at(
    file: &std::fs::File,
    mut buf: &[u8],
    mut offset: u64,
) -> io::Result<()> {
    while !buf.is_empty() {
        let written = file.write_at(
            buf,
            offset,
        )?;

        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "failed to write whole buffer",
            ));
        }

        buf = &buf[written..];

        offset += written as u64;
    }

    Ok(())
}

// ============================================================================
// 加载 .state
//
// 格式：
//
// 0
// 2
// 3
//
// 表示 Range 0 / 2 / 3 已完成。
// ============================================================================

async fn load_completed_ranges(
    state_path: &str,
) -> anyhow::Result<HashSet<u64>> {
    let content = match tokio::fs::read_to_string(state_path).await {
        Ok(content) => content,

        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(HashSet::new());
        }

        Err(err) => return Err(err.into()),
    };

    let mut completed = HashSet::new();

    for line in content.lines() {
        if let Ok(index) = line.parse::<u64>() {
            completed.insert(index);
        }
    }

    Ok(completed)
}

// ============================================================================
// 计算之前已经完成多少字节
// ============================================================================

fn calculate_completed_bytes(
    completed: &HashSet<u64>,
    total_size: u64,
    range_size: u64,
) -> u64 {
    let mut bytes = 0;

    for index in completed {
        let start = index * range_size;

        if start >= total_size {
            continue;
        }

        let end = ((index + 1) * range_size - 1)
            .min(total_size - 1);

        bytes += end - start + 1;
    }

    bytes
}

// ============================================================================
// 打印进度
// ============================================================================

fn print_progress(
    downloaded: u64,
    total_size: u64,
    start_time: Instant,
    last_downloaded: u64,
    last_tick: Instant,
) {
    let downloaded_mib =
        downloaded as f64 / 1024.0 / 1024.0;

    let total_mib =
        total_size as f64 / 1024.0 / 1024.0;

    let percent =
        downloaded as f64 / total_size as f64 * 100.0;

    // ---------------------------------------------------------
    // 本次程序启动后的平均速度
    //
    // 注意 resumed_bytes 会算进 downloaded，
    // 所以严格来说这一版 resume 后 avg_speed 不够准确。
    // 下一版再处理。
    // ---------------------------------------------------------

    let elapsed =
        start_time.elapsed().as_secs_f64();

    let avg_speed =
        if elapsed > 0.0 {
            downloaded_mib / elapsed
        } else {
            0.0
        };

    // ---------------------------------------------------------
    // 最近一个刷新窗口的速度
    // ---------------------------------------------------------

    let window_bytes =
        downloaded.saturating_sub(last_downloaded);

    let window_seconds =
        last_tick.elapsed().as_secs_f64();

    let current_speed =
        if window_seconds > 0.0 {
            window_bytes as f64
                / 1024.0
                / 1024.0
                / window_seconds
        } else {
            0.0
        };

    let remaining_mib =
        (total_mib - downloaded_mib)
            .max(0.0);

    let eta =
        if current_speed > 0.0 {
            remaining_mib / current_speed
        } else {
            0.0
        };

    print!(
        "\rprogress: {:6.2}% | {:6.2}/{:.2} MiB | \
         {:6.2} MiB/s | avg {:6.2} MiB/s | ETA {:.1}s",
        percent,
        downloaded_mib,
        total_mib,
        current_speed,
        avg_speed,
        eta,
    );

    io::stdout()
        .flush()
        .unwrap();
}