use serde::Deserialize;

#[derive(Deserialize, Debug)]
struct Post {
    id: u32,
    title: String,
}

fn main() -> Result<(), ureq::Error> {
    // Simple GET request
    let body: String = ureq::get("https://httpbin.org").call()?.into_string()?;
    println!("GET Response:\n{}", body);

    // JSON POST request with deserialization
    let json_res: Post = ureq::post("https://typicode.com")
        .send_json(ureq::json!({
            "title": "Hello Rust",
            "body": "No Tokio required!",
            "userId": 1
        }))?
        .into_json()?;

    println!("POST Response struct: {:?}", json_res);
    Ok(())
}
