use std::env;

use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() != 2 {
        println!("usage: rget <url>");
        return Ok(());
    }

    let url = &args[1];

    println!("downloading: {url}");

    let response = reqwest::get(url).await?;

    println!("status: {}", response.status());

    let mut file = tokio::fs::File::create("download.bin").await?;

    let mut stream = response.bytes_stream();

    let mut downloaded: u64 = 0;

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result?;

        file.write_all(&chunk).await?;

        downloaded += chunk.len() as u64;

        println!("downloaded: {downloaded} bytes");
    }

    file.flush().await?;

    println!("download finished: {downloaded} bytes");

    Ok(())
}