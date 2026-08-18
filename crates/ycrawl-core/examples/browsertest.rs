use std::time::Duration;
use ycrawl_core::{Browser, FetchedBody};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let url = std::env::args().nth(1).expect("usage: browsertest <url>");
    eprintln!("launching geckodriver…");
    let b = Browser::launch(4477).await?;
    eprintln!("launched. fetching {url}");
    let f = b
        .fetch(&url, Duration::from_secs(30), Duration::from_millis(1500))
        .await?;
    eprintln!(
        "got {} bytes in {}ms, final_url={}",
        f.source_bytes, f.elapsed_ms, f.final_url
    );
    let text: String = match f.body {
        FetchedBody::Html(html) | FetchedBody::Text(html) => html.chars().take(300).collect(),
        FetchedBody::Pdf(_) => "[PDF]".into(),
    };
    eprintln!("head: {text}");
    Ok(())
}
