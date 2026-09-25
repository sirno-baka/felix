use reqwest::{Client, Url};
use serde::Deserialize;
use serde_json::json;
use std::error::Error;
use std::net::ToSocketAddrs;
use std::time::Instant;


fn probe_dns(host: &str, port: u16) {
    println!("[net:probe] resolving {host}:{port}");
    let started = Instant::now();
    match (host, port).to_socket_addrs() {
        Ok(addresses) => println!(
            "[net:probe] DNS OK host={host} elapsed={}ms addresses={:?}",
            started.elapsed().as_millis(),
            addresses.collect::<Vec<_>>()
        ),
        Err(error) => println!(
            "[net:probe] DNS FAILED host={host} elapsed={}ms error={error}",
            started.elapsed().as_millis()
        ),
    }
}

const CLIENT_ID: &str = "kimne78kx3ncx6brgo4mv6wki5h1ko";
const PLAYBACK_QUERY: &str = "ed230aa1e33e07eebb8928504583da78a5173989fadfb1ac94be06a04f3cdbe9";

#[derive(Deserialize)]
struct GraphQlResponse {
    data: GraphQlData,
}

#[derive(Deserialize)]
struct GraphQlData {
    #[serde(rename = "streamPlaybackAccessToken")]
    stream_token: Option<AccessToken>,
}

#[derive(Deserialize)]
struct AccessToken {
    value: String,
    signature: String,
}

pub async fn master_playlist(client: &Client, channel: &str) -> Result<String, Box<dyn Error>> {
    let request = json!({
        "operationName": "PlaybackAccessToken",
        "extensions": { "persistedQuery": { "version": 1, "sha256Hash": PLAYBACK_QUERY } },
        "variables": {
            "isLive": true,
            "login": channel,
            "isVod": false,
            "vodID": "",
            "playerType": "embed",
            "platform": "site"
        }
    });
    probe_dns("gql.twitch.tv", 443);
    println!("[twitch:gql] POST gql.twitch.tv/gql channel={channel}");
    println!("[twitch:gql] entering reqwest send() (DNS/connect/TLS/request write/response headers)");
    let started = Instant::now();
    let response = client
        .post("https://gql.twitch.tv/gql")
        .header("Client-Id", CLIENT_ID)
        .json(&request)
        .send()
        .await?;
    println!(
        "[twitch:gql] response headers status={} elapsed={}ms",
        response.status(),
        started.elapsed().as_millis()
    );
    let response = response.error_for_status()?;
    println!("[twitch:gql] reading response body");
    let bytes = response.bytes().await?;
    println!(
        "[twitch:gql] body complete bytes={} elapsed={}ms",
        bytes.len(),
        started.elapsed().as_millis()
    );
    let body: GraphQlResponse = serde_json::from_slice(&bytes)?;
    let token = body.data.stream_token.ok_or("channel is offline or Twitch returned no playback token")?;
    println!(
        "[twitch:gql] playback token parsed value_len={} sig_len={}",
        token.value.len(),
        token.signature.len()
    );

    let mut url = Url::parse(&format!("https://usher.ttvnw.net/api/v2/channel/hls/{channel}.m3u8"))?;
    url.query_pairs_mut()
        .append_pair("client_id", CLIENT_ID)
        .append_pair("token", &token.value)
        .append_pair("sig", &token.signature)
        .append_pair("platform", "web")
        .append_pair("p", "1")
        .append_pair("allow_source", "true")
        .append_pair("allow_audio_only", "true")
        .append_pair("playlist_include_framerate", "true")
        .append_pair("multigroup_video", "true")
        .append_pair("supported_codecs", "h264");
    probe_dns("usher.ttvnw.net", 443);
    println!("[twitch:master] GET usher.ttvnw.net{}", url.path());
    println!("[twitch:master] entering reqwest send()");
    let started = Instant::now();
    let response = client.get(url).send().await?;
    let status = response.status();
    println!(
        "[twitch:master] response headers status={} elapsed={}ms",
        status,
        started.elapsed().as_millis()
    );
    println!("[twitch:master] reading response body");
    let bytes = response.bytes().await?;
    println!(
        "[twitch:master] body complete bytes={} elapsed={}ms",
        bytes.len(),
        started.elapsed().as_millis()
    );
    if !status.is_success() {
        let body = String::from_utf8_lossy(&bytes);
        println!(
            "[twitch:master] error body status={} bytes={} body={}",
            status,
            bytes.len(),
            body
        );
        return Err(format!("Twitch usher returned {status}: {body}").into());
    }
    Ok(String::from_utf8(bytes.to_vec())?)
}

pub fn resolve_url(base: &Url, uri: &str) -> Result<Url, Box<dyn Error>> {
    Ok(base.join(uri)?)
}
