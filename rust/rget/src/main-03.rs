use std::env;

use tokio::io::AsyncWriteExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() != 2 {
        println!("usage: rget <url>");
        return Ok(());
    }

    let url = &args[1];

    let client = reqwest::Client::new();

    // 1. 先发 HEAD，获取文件大小
    let head_response = client.head(url).send().await?;

    println!("HEAD status: {}", head_response.status());

    let total_size = head_response
        .content_length()
        .unwrap_or(0);

    println!("total size: {} bytes", total_size);

    // 2. 再发 GET，真正下载
    let mut response = client.get(url).send().await?;

    println!("GET status: {}", response.status());

    let mut file = tokio::fs::File::create("download.bin").await?;

    let mut downloaded: u64 = 0;

    while let Some(chunk) = response.chunk().await? {
        file.write_all(&chunk).await?;

        downloaded += chunk.len() as u64;

        if total_size > 0 {
            let percent =
                downloaded as f64 / total_size as f64 * 100.0;

            println!(
                "downloaded: {} / {} bytes ({:.2}%)",
                downloaded,
                total_size,
                percent
            );
        } else {
            println!("downloaded: {} bytes", downloaded);
        }
    }

    file.flush().await?;

    println!("download finished");

    Ok(())
}