use std::env::args;
use std::io::{Bytes, Read, Write};
use std::net::Shutdown;
use std::net::TcpStream;
use std::net::{Ipv4Addr, SocketAddrV4};

const ADDR: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 2);
const PORT: u16 = 6666;

fn main() -> std::io::Result<()> {
    println!("Hello Client!");
    println!("213");

    match TcpStream::connect(SocketAddrV4::new(ADDR, PORT)) {
        Ok(mut stream) => {
            println!(
                "Connected to the server on {:?}",
                stream.peer_addr().unwrap()
            );

            let message = "hello".to_string();
            match message.as_str() {
                "#END#" => stream.shutdown(Shutdown::Both).expect("Shutdown Failed!"),
                _ => {
                    print!("SENT!");
                    stream.write(&message.into_bytes())?;
                    //stream.read(&mut [0; 128])?;
                }
            }
        }
        Err(err) => {
            println!("Error: {}", err);
            println!("Couldn't connect to server...");
        }
    }

    Ok(())
}
