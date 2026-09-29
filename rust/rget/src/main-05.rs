use std::env;
use std::os::unix::fs::FileExt;
use std::sync::Arc;

use reqwest::header::{CONTENT_LENGTH, RANGE};

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

    for i in 0..concurrency {
        let start = i * range_size;

        if start >= total_size {
            break;
        }

        let end = ((i + 1) * range_size - 1)
            .min(total_size - 1);

        println!("range {i}: {start}-{end}");

        let client = client.clone();
        let url = url.clone();
        let file = Arc::clone(&file);

        let handle = tokio::spawn(async move {
            download_range(
                client,
                url,
                file,
                i,
                start,
                end,
            )
            .await
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.await??;
    }

    println!("download finished");

    Ok(())
}

async fn download_range(
    client: reqwest::Client,
    url: String,
    file: Arc<std::fs::File>,
    index: u64,
    start: u64,
    end: u64,
) -> anyhow::Result<()> {
    println!("task {index} started");

    let range = format!("bytes={start}-{end}");

    let mut response = client
        .get(&url)
        .header(RANGE, range)
        .send()
        .await?;

    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        anyhow::bail!(
            "server does not support range: {}",
            response.status()
        );
    }

    let mut offset = start;
    let mut received: u64 = 0;

    while let Some(chunk) = response.chunk().await? {
        let file = Arc::clone(&file);

        let chunk_len = chunk.len() as u64;

        let write_offset = offset;

        // write_at 是 blocking syscall
        tokio::task::spawn_blocking(move || {
            write_all_at(&file, &chunk, write_offset)
        })
        .await??;

        offset += chunk_len;
        received += chunk_len;
    }

    let expected = end - start + 1;

    if received != expected {
        anyhow::bail!(
            "task {index}: length mismatch, expected={expected}, actual={received}"
        );
    }

    println!(
        "task {index} finished: {received} bytes"
    );

    Ok(())
}

fn write_all_at(
    file: &std::fs::File,
    mut buf: &[u8],
    mut offset: u64,
) -> std::io::Result<()> {
    while !buf.is_empty() {
        let written = file.write_at(buf, offset)?;

        if written == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "failed to write whole buffer",
            ));
        }

        buf = &buf[written..];
        offset += written as u64;
    }

    Ok(())
}