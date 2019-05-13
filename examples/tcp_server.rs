extern crate bytes;
extern crate futures;
extern crate tokio;
extern crate tokio_simplified;

use bytes::BytesMut;
use futures::Stream;
use std::net::{IpAddr, Ipv4Addr};
use tokio::codec::{Decoder, Encoder};
use tokio::net::{TcpListener, TcpStream};

use tokio_simplified::IoManagerBuilder;

struct LineCodec;

impl Decoder for LineCodec {
    type Item = String;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let line_end_index = src.iter().position(|x| x.clone() == '\n' as u8);
        Ok(match line_end_index {
            None => None,
            Some(index) => {
                let line = src.split_to(index);
                src.split_to(1);
                Some(String::from_utf8(line.to_vec()).unwrap())
            }
        })
    }

    fn decode_eof(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() > 0 {
            return Ok(Some(String::from_utf8(src.to_vec()).unwrap()));
        }
        Err(std::io::Error::from(std::io::ErrorKind::ConnectionAborted))
    }
}

impl Encoder for LineCodec {
    type Item = String;
    type Error = std::io::Error;

    fn encode(&mut self, item: Self::Item, dst: &mut BytesMut) -> Result<(), Self::Error> {
        dst.extend(item.as_bytes());
        dst.extend(b"\r\n");
        Ok(())
    }
}

fn process_socket(socket: TcpStream) {
    println!("New Client");
    let (sink, stream) = LineCodec.framed(socket).split();
    let trx = IoManagerBuilder::new(sink, stream)
        .with_filter(|frame, writer| {
            if frame.to_lowercase().contains("hello there") {
                writer.write("General Kenobi!".into());
                return None;
            }
            Some(frame)
        })
        .with_error_handler(move |error| {
            println!("{}", error);
        })
        .build();
    let mut writer = trx.get_writer();
    trx.on_receive(move |frame| {
        println!("Got frame: {}", frame);
        writer.write("Hi there".into());
        Ok(())
    });
}

fn main() {
    println!("Hello Tokio");
    let addr = std::net::SocketAddr::new(IpAddr::from(Ipv4Addr::new(0, 0, 0, 0)), 6000);
    let listener = TcpListener::bind(&addr);
    match listener {
        Ok(listener) => tokio::run(
            listener
                .incoming()
                .map_err(|e| eprintln!("failed to accept socket; error = {:?}", e))
                .for_each(|socket| {
                    process_socket(socket);
                    Ok(())
                }),
        ),
        _ => {}
    };
}
