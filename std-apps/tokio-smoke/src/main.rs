use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

static NEXT_CLIENT_ID: AtomicUsize = AtomicUsize::new(1);

async fn write_all(stream: &tokio::net::TcpStream, data: &[u8]) -> io::Result<()> {
    let mut written = 0usize;
    while written < data.len() {
        stream.writable().await?;
        match stream.try_write(&data[written..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "zero-length TCP write",
                ));
            }
            Ok(n) => written += n,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

async fn run_http_client() -> io::Result<()> {
    const SERVER: &str = "10.0.2.2:18081";
    const REQUEST: &[u8] = b"GET / HTTP/1.0\r\nHost: 10.0.2.2\r\nConnection: close\r\n\r\n";

    println!("tokio-client: connecting to {SERVER}");
    let stream = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::TcpStream::connect(SERVER),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connect timed out"))??;

    println!(
        "tokio-client: connected local={} peer={}",
        stream.local_addr()?,
        stream.peer_addr()?
    );

    write_all(&stream, REQUEST).await?;
    println!("tokio-client: HTTP request sent");

    let mut total = 0usize;
    let mut first = [0u8; 1024];
    loop {
        stream.readable().await?;
        match stream.try_read(&mut first[total..]) {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                if total == first.len() {
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            Err(error) => return Err(error),
        }
    }

    println!("tokio-client: received {total} bytes");
    match std::str::from_utf8(&first[..total]) {
        Ok(text) => println!("tokio-client: response:\n{text}"),
        Err(_) => println!("tokio-client: response is not UTF-8"),
    }
    println!("tokio-client: PASS");
    Ok(())
}

async fn echo_client(
    id: usize,
    stream: tokio::net::TcpStream,
    peer: std::net::SocketAddr,
) -> io::Result<()> {
    let local = stream.local_addr()?;
    println!("tokio: client #{id} connected peer={peer} local={local}");

    let mut buf = [0u8; 2048];

    loop {
        let n = loop {
            stream.readable().await?;
            match stream.try_read(&mut buf) {
                Ok(n) => break n,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                Err(error) => return Err(error),
            }
        };

        if n == 0 {
            println!("tokio: client #{id} disconnected");
            return Ok(());
        }

        println!("tokio: client #{id} received {n} bytes");

        write_all(&stream, &buf[..n]).await?;

        println!("tokio: client #{id} echoed {n} bytes");
    }
}

fn spawn_client(stream: tokio::net::TcpStream, peer: std::net::SocketAddr) {
    let id = NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed);
    tokio::spawn(async move {
        if let Err(error) = echo_client(id, stream, peer).await {
            println!("tokio: client #{id} error: {error:?}");
        }
    });
}

fn main() -> io::Result<()> {
    println!("tokio: creating runtime");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;
    println!("tokio: runtime ready");

    runtime.block_on(async {
        if std::env::args().nth(1).as_deref() == Some("client") {
            return run_http_client().await;
        }

        println!("tokio: binding 0.0.0.0:8080");
        let listener = match tokio::net::TcpListener::bind("0.0.0.0:8080").await {
            Ok(listener) => listener,
            Err(error) => {
                println!("tokio: bind failed: {error:?}");
                return Err(error);
            }
        };
        println!("tokio: listening on {}", listener.local_addr()?);

        // Preserve the original timer/readiness smoke test. If a client happens
        // to arrive during this short window, hand it to the normal task path.
        match tokio::time::timeout(Duration::from_millis(100), listener.accept()).await {
            Err(_) => println!("tokio: poll timeout works"),
            Ok(Ok((stream, peer))) => {
                println!("tokio: client arrived during timeout test");
                spawn_client(stream, peer);
            }
            Ok(Err(error)) => return Err(error),
        }

        println!("tokio: multi-client echo server ready on port 8080");

        loop {
            let (stream, peer) = listener.accept().await?;
            println!("tokio: accepted {peer}");
            spawn_client(stream, peer);
        }
    })
}
