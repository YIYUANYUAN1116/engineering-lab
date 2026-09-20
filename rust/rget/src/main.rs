use std::env;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() != 2 {
        println!("Usage: rget <url>");
        return Ok(());
    }

    let url = &args[1];

    println!("Downloading: {}", url);

    let response = reqwest::get(url).await?;

    println!("Status: {}", response.status());

    let body = response.bytes().await?;


    tokio::fs::write("download.bin", &body).await?;

    println!("Downloaded {} bytes to download.bin", body.len());

    Ok(())

}
