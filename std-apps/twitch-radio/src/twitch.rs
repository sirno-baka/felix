use reqwest::{Client, Url};
use serde::Deserialize;
use serde_json::json;
use std::error::Error;

const CLIENT_ID: &str = "kimne78kx3ncx6brgo4mv6wki5h1ko";
const PLAYBACK_QUERY: &str = "0828119ded1c13477966434e15800ff57ddacf13ba1911c129dc2200705b0712";

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
            "playerType": "embed"
        }
    });
    let response = client
        .post("https://gql.twitch.tv/gql")
        .header("Client-Id", CLIENT_ID)
        .json(&request)
        .send()
        .await?
        .error_for_status()?;
    let body: GraphQlResponse = response.json().await?;
    let token = body.data.stream_token.ok_or("channel is offline or Twitch returned no playback token")?;

    let mut url = Url::parse(&format!("https://usher.ttvnw.net/api/channel/hls/{channel}.m3u8"))?;
    url.query_pairs_mut()
        .append_pair("client_id", CLIENT_ID)
        .append_pair("token", &token.value)
        .append_pair("sig", &token.signature)
        .append_pair("allow_source", "true")
        .append_pair("allow_audio_only", "true")
        .append_pair("playlist_include_framerate", "true");
    Ok(client.get(url).send().await?.error_for_status()?.text().await?)
}

pub fn resolve_url(base: &Url, uri: &str) -> Result<Url, Box<dyn Error>> {
    Ok(base.join(uri)?)
}
