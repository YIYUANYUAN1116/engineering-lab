use std::env;

use reqwest::header::RANGE;
use tokio::io::AsyncWriteExt;
use reqwest::header::CONTENT_LENGTH;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() != 2 {
        println!("usage: rget <url>");
        return Ok(());
    }

    let url = &args[1];

    let client = reqwest::Client::new();

    // 1. HEAD 获取文件总大小
    let head_response = client.head(url).send().await?;

    println!("HEAD status: {}", head_response.status());
    println!("HEAD headers: {:#?}", head_response.headers());

    let total_size = head_response
        .headers()
        .get(CONTENT_LENGTH)
        .ok_or_else(|| anyhow::anyhow!("missing Content-Length"))?
        .to_str()?
        .parse::<u64>()?;

    println!("total size: {total_size} bytes");

    // 2. 固定使用 4 个并发下载任务
    let concurrency: u64 = 4;

    let chunk_size = total_size.div_ceil(concurrency);

    println!("range size: {chunk_size} bytes");

    let mut handles = Vec::new();

    // 3. 为每一段创建一个 Tokio task
    for i in 0..concurrency {
        let start = i * chunk_size;

        if start >= total_size {
            break;
        }

        let end = ((i + 1) * chunk_size - 1).min(total_size - 1);

        println!("range {i}: {start}-{end}");

        let client = client.clone();
        let url = url.clone();

        let handle = tokio::spawn(async move { download_range(client, url, i, start, end).await });

        handles.push(handle);
    }

    // 4. 等所有 range 下载完成
    let mut parts = Vec::new();

    for handle in handles {
        let part = handle.await??;
        parts.push(part);
    }

    // 5. 按 range 顺序排列
    parts.sort_by_key(|part| part.index);

    // 6. 合并写入文件
    let mut file = tokio::fs::File::create("download.bin").await?;

    for part in parts {
        file.write_all(&part.data).await?;
    }

    file.flush().await?;

    println!("download finished");

    Ok(())
}

struct DownloadPart {
    index: u64,
    data: bytes::Bytes,
}

async fn download_range(
    client: reqwest::Client,
    url: String,
    index: u64,
    start: u64,
    end: u64
) -> anyhow::Result<DownloadPart> {
    println!("task {index} started");

    let range = format!("bytes={start}-{end}");

    let response = client.get(&url).header(RANGE, range).send().await?;

    println!("task {index}: status = {}", response.status());

    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        anyhow::bail!("server does not support Range, status={}", response.status());
    }

    let data = response.bytes().await?;

    println!("task {index} finished: {} bytes", data.len());

    Ok(DownloadPart {
        index,
        data,
    })
}
