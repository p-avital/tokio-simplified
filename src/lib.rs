/// # A Simplified API to work with Tokio's SplitSink and SplitStream
///
/// ## Motivation
/// Although Tokio is extremely powerful, somme of its features have been less than intuitive to me.
/// So I built this crate to simplify interracting with Tokio in the ways that I usually do:
/// * Writing to an IO without really wanting to do much with what happens then
/// * Subscribing one or several callbacks to an IO.
///
/// ## Usage
/// This API should only be used from inside a Tokio Runtime: it will try to spawn Tokio Tasks and will thus panic if it's not the case.
///
/// ### Standard Usage: Multiple Callbacks
/// ```rust
/// fn tokio_main() {
///     let (sink, stream) = LineCodec.framed(tcp_stream).split();
///     let io = AsyncReadWriter(sink, stream, None);
///     let writer = io.get_writer();
///     io.subscribe(move |frame| {
///         writer.write(frame);
///     });
///     io.subscribe(move |frame| {
///         println!("{}", frame);
///     })
/// }
/// ```
///
/// ### Filtering
/// You can use filters to have your callbacks only be called when the frame matches some criterion.
/// ```rust
/// fn tokio_main() {
///     let (sink, stream) = LineCodec.framed(tcp_stream).split();
///     let io = AsyncReadWriter(sink, stream, Some(|frame, writer| {
///         if frame.to_ascii_lowercase().contains("hello there") {
///             writer.write("General Kenobi!");
///             return None;
///         }
///         Some(frame)
///     }));
///     let writer = io.get_writer();
///     io.subscribe(move |frame| {
///         writer.write(frame);
///     });
///     io.subscribe(move |frame| {
///         println!("{}", frame);
///     })
/// }
/// ```
///
/// ### Single Callback Tip
/// Every time you use `subscribe(callback)`, you endure the cost of one more futures::sync::mpsc::channel,
/// and of one frame.clone() per callback call.
/// It's not a high cost, but if you only have one callback, you can cut these costs by passing your callback
/// as a filter that always returns `None`.
/// ```rust
/// fn tokio_main() {
///     let (sink, stream) = LineCodec.framed(tcp_stream).split();
///     let io = AsyncReadWriter(sink, stream, Some(|frame, writer| {
///         writer.write(frame);
///         None
///     }));
/// }
/// ```
extern crate tokio;

use futures::sync::mpsc::channel;
use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use tokio::codec::{Decoder, Encoder, Framed};
use tokio::prelude::*;

/// A simple interface to interact with a tokio sink.
///
/// Should always be constructed by a call to some AsyncReadWriter's get_writer().
pub struct AsyncWriter<Codec>
where
    Codec: Encoder,
{
    tx: RefCell<futures::sync::mpsc::Sender<<Codec as Encoder>::Item>>,
}

impl<Codec> AsyncWriter<Codec>
where
    Codec: Encoder,
{
    /// Forwards the frame to the tokio sink associated with the AsyncReadWriter that build this instance.
    pub fn write(
        &self,
        frame: <Codec as Encoder>::Item,
    ) -> Result<
        futures::AsyncSink<<Codec as Encoder>::Item>,
        futures::sync::mpsc::SendError<<Codec as Encoder>::Item>,
    > {
        self.tx.borrow_mut().start_send(frame)
    }
}

/// A simplified interface to interact with tokio's streams and sinks.
///
/// Allows easy subscription to the stream's frames, and easy sending to the sink.
pub struct AsyncReadWriter<Codec>
where
    Codec: Encoder + Decoder,
{
    tx: futures::sync::mpsc::Sender<<Codec as Encoder>::Item>,
    subscribers: Arc<Mutex<Vec<futures::sync::mpsc::Sender<<Codec as Decoder>::Item>>>>,
}

impl<Codec> AsyncReadWriter<Codec>
where
    Codec: Decoder + Encoder + std::marker::Send + 'static,
    <Codec as Encoder>::Item: std::marker::Send,
    <Codec as Encoder>::Error: std::marker::Send,
    <Codec as Decoder>::Item: std::marker::Send + Clone,
    <Codec as Decoder>::Error: std::marker::Send,
{
    /// SHOULD ALWAYS BE CALLED FROM INSIDE A TOKIO RUNTIME!
    ///
    /// Builds a new AsyncReadWriter from the provided sink and stream with no filter.
    ///
    /// You can provide a filter to run on each frame before sending said frames to callbacks.
    /// To provide a filter, use `with_filter(sink, stream, Some(Callback))`.
    pub fn new<Io>(
        sink: stream::SplitSink<Framed<Io, Codec>>,
        stream: stream::SplitStream<Framed<Io, Codec>>,
    ) -> Self
    where
        Io: AsyncWrite + AsyncRead + std::marker::Send + 'static,
    {
        Self::with_filter(
            sink,
            stream,
            None::<
                (fn(
                    <Codec as Decoder>::Item,
                    &AsyncWriter<Codec>,
                ) -> Option<<Codec as Decoder>::Item>),
            >,
        )
    }

    /// SHOULD ALWAYS BE CALLED FROM INSIDE A TOKIO RUNTIME!
    ///
    /// Builds a new AsyncReadWriter from the provided sink and stream.
    /// You can provide a filter to run on each frame before sending said frames to callbacks.
    ///
    /// Callbacks will not be called if the filter returned None, so if you intend on only having a single callback,
    /// using `filter=Some(callback)` with a callback that always returns `None` will save you the cost of the multiple
    /// callbacks handling provided by the `subscibe(callback)` API
    pub fn with_filter<Io, F>(
        sink: stream::SplitSink<Framed<Io, Codec>>,
        stream: stream::SplitStream<Framed<Io, Codec>>,
        mut filter: Option<F>,
    ) -> Self
    where
        Io: AsyncWrite + AsyncRead + std::marker::Send + 'static,
        F: FnMut(<Codec as Decoder>::Item, &AsyncWriter<Codec>) -> Option<<Codec as Decoder>::Item>
            + std::marker::Send
            + 'static,
    {
        let (sink_tx, sink_rx) = channel::<<Codec as Encoder>::Item>(10);
        let sink_task = sink_rx.forward(sink.sink_map_err(|_| ())).map(|_| ());
        tokio::spawn(sink_task);
        let filter_writer = AsyncWriter {
            tx: RefCell::new(sink_tx.clone()),
        };

        let subscribers = Arc::new(Mutex::new(Vec::<
            futures::sync::mpsc::Sender<<Codec as Decoder>::Item>,
        >::new()));
        let stream_subscribers_reference = subscribers.clone();
        let stream_task = stream
            .for_each(move |frame: <Codec as Decoder>::Item| {
                let frame = match &mut filter {
                    None => Some(frame),
                    Some(function) => function(frame, &filter_writer),
                };
                match frame {
                    Some(frame) => {
                        for tx in stream_subscribers_reference.lock().unwrap().iter_mut() {
                            match tx.start_send(frame.clone()) {
                                Ok(_) => {}
                                Err(error) => {
                                    eprintln!("Stream Subscriber Error: {}", error);
                                }
                            };
                        }
                    }
                    None => {}
                }
                Ok(())
            })
            .map_err(|_| ());
        tokio::spawn(stream_task);
        AsyncReadWriter {
            tx: sink_tx,
            subscribers,
        }
    }

    /// deprecated: use on_receive() instead, unless you NEED an mpsc::Sender to be notified.
    ///
    /// subscriber will receive any data polled from the internal stream.
    pub fn subscribe_mpsc_sender(
        &self,
        subscriber: futures::sync::mpsc::Sender<<Codec as Decoder>::Item>,
    ) {
        self.subscribers.lock().unwrap().push(subscriber);
    }


    /// callback will be called for each frame polled from the internal stream.
    pub fn on_receive<F>(&self, callback: F)
    where
        F: FnMut(<Codec as Decoder>::Item) -> Result<(), ()> + std::marker::Send + 'static,
        <Codec as Decoder>::Item: std::marker::Send + 'static,
    {
        let (tx, rx): (
            futures::sync::mpsc::Sender<<Codec as Decoder>::Item>,
            futures::sync::mpsc::Receiver<<Codec as Decoder>::Item>,
        ) = channel::<<Codec as Decoder>::Item>(10);
        let on_frame = rx.for_each(callback).map(|_| ());
        tokio::spawn(on_frame);
        self.subscribe_mpsc_sender(tx);
    }

    /// Returns an AsyncWriter that will forward data to the associated tokio sink.
    pub fn get_writer(&self) -> AsyncWriter<Codec> {
        AsyncWriter {
            tx: RefCell::new(self.tx.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate bytes;
    use crate::*;
    use bytes::BytesMut;
    use std::net::ToSocketAddrs;
    use tokio::codec::{Decoder, Encoder};

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
    }

    impl Encoder for LineCodec {
        type Item = String;
        type Error = std::io::Error;

        fn encode(&mut self, item: Self::Item, dst: &mut BytesMut) -> Result<(), Self::Error> {
            dst.extend(item.as_bytes());
            dst.extend(b"\n");
            Ok(())
        }
    }
    #[test]
    fn tcp_test() {
        let address_str = "portquiz.net:6000";
        let address = address_str.to_socket_addrs();
        match address {
            Ok(mut addresses) => {
                match addresses.next() {
                    Some(address) => {
                        println!("Address {} resolved to {}", &address_str, &address);
                        let task =
                            tokio::net::TcpStream::connect(&address).then(|stream| match stream {
                                Ok(stream) => {
                                    let (sink, stream) = LineCodec.framed(stream).split();
                                    let trx = AsyncReadWriter::new(sink, stream);
                                    trx.on_receive(move |string| {
                                        println!("Received: {}", &string);
                                        Ok(())
                                    });
                                    Ok(())
                                }
                                Err(_) => Err(()),
                            });
                        tokio::run(task);
                    }
                    None => {
                        eprintln!("Could not resolve {}", &address_str);
                    }
                };
            }
            Err(error) => {
                eprintln!("{}", error);
            }
        }
    }
}
