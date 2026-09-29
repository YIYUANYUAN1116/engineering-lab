use std::env;
use std::os::unix::fs::FileExt;
use std::sync::Arc;
use tokio::sync::{ mpsc, watch };
use reqwest::header::{ CONTENT_LENGTH, RANGE };
use std::io::{ self, Write };
use std::time::Instant;
use tokio::time::{ self, Duration };

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() != 2 {
        println!("usage: rget <url>");
        return Ok(());
    }

    let url = &args[1];

    let client = reqwest::Client::new();

    // 1. HEAD 获取文件大小
    let head_response = client.head(url).send().await?;

    let total_size = head_response
        .headers()
        .get(CONTENT_LENGTH)
        .ok_or_else(|| anyhow::anyhow!("missing Content-Length"))?
        .to_str()?
        .parse::<u64>()?;

    println!("total size: {total_size} bytes");

    // 2. 提前创建目标文件
    let file = std::fs::File::create("download.bin")?;

    // 直接把文件扩到最终大小
    file.set_len(total_size)?;

    // 多个 task 共同持有这个 File
    let file = Arc::new(file);

    let concurrency: u64 = 4;

    let range_size = total_size.div_ceil(concurrency);

    let mut handles = Vec::new();

    let (tx, mut rx) = mpsc::channel::<u64>(100);

    let (cancel_tx, cancel_rx) = watch::channel(false);

    let ctrl_c_cancel_tx = cancel_tx.clone();

    let ctrl_c_handle = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            println!("\ncancelling...");
            let _ = ctrl_c_cancel_tx.send(true);
        }
    });

    let progress_handle = tokio::spawn(async move {
        let start_time = Instant::now();

        let mut downloaded: u64 = 0;

        // 用于计算最近一个窗口的速度
        let mut last_downloaded: u64 = 0;
        let mut last_tick = Instant::now();

        let mut interval = time::interval(Duration::from_millis(500));

        loop {
            tokio::select! {
            // 分支 1：收到 worker 发来的新进度
            message = rx.recv() => {
                match message {
                    Some(bytes) => {
                        downloaded += bytes;
                    }

                    // 所有 Sender 都已经 drop
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

            // 分支 2：500ms 到了，刷新一次终端
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

    for i in 0..concurrency {
        let start = i * range_size;

        if start >= total_size {
            break;
        }

        let end = ((i + 1) * range_size - 1).min(total_size - 1);

        println!("range {i}: {start}-{end}");

        let client = client.clone();
        let url = url.clone();
        let file = Arc::clone(&file);

        let tx = tx.clone();
        let mut cancel_rx = cancel_rx.clone();

        let handle = tokio::spawn(async move {
            download_range(client, url, file, tx, cancel_rx, i, start, end).await
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.await??;
    }

    drop(tx);

    progress_handle.await?;

    ctrl_c_handle.abort();

    println!("download finished");

    Ok(())
}

async fn download_range(
    client: reqwest::Client,
    url: String,
    file: Arc<std::fs::File>,
    tx: mpsc::Sender<u64>,
    mut cancel_rx: watch::Receiver<bool>,
    index: u64,
    start: u64,
    end: u64
) -> anyhow::Result<()> {
    let range = format!("bytes={start}-{end}");

    let mut response = client.get(&url).header(RANGE, range).send().await?;

    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        anyhow::bail!("server does not support range: {}", response.status());
    }

    let mut offset = start;
    let mut received: u64 = 0;

    loop {
        let chunk =
            tokio::select! {
        result = response.chunk() => {
            match result? {
                Some(chunk) => chunk,
                None => break,
            }
        }

        result = cancel_rx.changed() => {
            // Sender 被释放
            if result.is_err() {
                break;
            }

            if *cancel_rx.borrow() {
                println!("task {index} cancelled");
                return Ok(());
            }

            continue;
        }
    };

        let chunk_len = chunk.len() as u64;

        let file = Arc::clone(&file);

        tokio::task::spawn_blocking(move || { write_all_at(&file, &chunk, offset) }).await??;

        offset += chunk_len;
        received += chunk_len;

        let _ = tx.send(chunk_len).await;
    }

    Ok(())
}

fn write_all_at(file: &std::fs::File, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
    while !buf.is_empty() {
        let written = file.write_at(buf, offset)?;

        if written == 0 {
            return Err(
                std::io::Error::new(std::io::ErrorKind::WriteZero, "failed to write whole buffer")
            );
        }

        buf = &buf[written..];
        offset += written as u64;
    }

    Ok(())
}

fn print_progress(
    downloaded: u64,
    total_size: u64,
    start_time: Instant,
    last_downloaded: u64,
    last_tick: Instant
) {
    let downloaded_mib = (downloaded as f64) / 1024.0 / 1024.0;

    let total_mib = (total_size as f64) / 1024.0 / 1024.0;

    let percent = ((downloaded as f64) / (total_size as f64)) * 100.0;

    // 平均速度
    let elapsed = start_time.elapsed().as_secs_f64();

    let avg_speed = if elapsed > 0.0 { downloaded_mib / elapsed } else { 0.0 };

    // 最近一个 500ms 窗口的速度
    let window_bytes = downloaded - last_downloaded;

    let window_seconds = last_tick.elapsed().as_secs_f64();

    let current_speed = if window_seconds > 0.0 {
        (window_bytes as f64) / 1024.0 / 1024.0 / window_seconds
    } else {
        0.0
    };

    let remaining_mib = total_mib - downloaded_mib;

    let eta = if current_speed > 0.0 { remaining_mib / current_speed } else { 0.0 };

    print!(
        "\rprogress: {:6.2}% | {:6.2}/{:.2} MiB | \
         {:6.2} MiB/s | avg {:6.2} MiB/s | ETA {:.1}s",
        percent,
        downloaded_mib,
        total_mib,
        current_speed,
        avg_speed,
        eta
    );

    io::stdout().flush().unwrap();
}
