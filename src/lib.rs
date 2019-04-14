extern crate tokio;

use futures::sync::mpsc::channel;
use std::cell::RefCell;
use std::sync::{Arc, Mutex};
use tokio::codec::{Decoder, Encoder, Framed};
use tokio::prelude::*;

/// A simple interface to interact with a tokio sink.
///
/// Should always be constructed by a call to some AsyncReadWriter's get_writer().
pub struct AsyncWriter<SendType> {
    tx: RefCell<futures::sync::mpsc::Sender<SendType>>,
}

impl<SendType> AsyncWriter<SendType> {
    /// Forwards the frame to the tokio sink associated with the AsyncReadWriter that build this instance.
    pub fn write(
        &self,
        frame: SendType,
    ) -> Result<futures::AsyncSink<SendType>, futures::sync::mpsc::SendError<SendType>> {
        self.tx.borrow_mut().start_send(frame)
    }
}

/// A simplified interface to interact with tokio's streams and sinks.
///
/// Allows easy subscription to the stream's frames, and easy sending to the sink.
pub struct AsyncReadWriter<SendType, ReceiveType> {
    tx: futures::sync::mpsc::Sender<SendType>,
    subscribers: Arc<Mutex<Vec<futures::sync::mpsc::Sender<ReceiveType>>>>,
}

impl<SendType, ReceiveType> AsyncReadWriter<SendType, ReceiveType> {
    /// SHOULD ALWAYS BE CALLED FROM INSIDE A TOKIO RUNTIME!
    ///
    /// Builds a new AsyncReadWriter from the provided sink and stream.
    pub fn new<Io, Codec>(
        sink: stream::SplitSink<Framed<Io, Codec>>,
        stream: stream::SplitStream<Framed<Io, Codec>>,
    ) -> AsyncReadWriter<<Codec as Encoder>::Item, <Codec as Decoder>::Item>
    where
        Codec: Decoder + Encoder + std::marker::Send + 'static,
        Io: AsyncWrite + AsyncRead + std::marker::Send + 'static,
        <Codec as Encoder>::Item: std::marker::Send,
        <Codec as Encoder>::Error: std::marker::Send,
        <Codec as Decoder>::Item: std::marker::Send + Clone,
        <Codec as Decoder>::Error: std::marker::Send,
    {
        let (sink_tx, sink_rx) = channel::<<Codec as Encoder>::Item>(10);
        let sink_task = sink_rx.forward(sink.sink_map_err(|_| ())).map(|_| ());
        tokio::spawn(sink_task);

        let subscribers = Arc::new(Mutex::new(Vec::<
            futures::sync::mpsc::Sender<<Codec as Decoder>::Item>,
        >::new()));
        let stream_subscribers_reference = subscribers.clone();
        let stream_task = stream
            .for_each(move |frame: <Codec as Decoder>::Item| {
                for tx in stream_subscribers_reference.lock().unwrap().iter_mut() {
                    match tx.start_send(frame.clone()) {
                        Ok(_) => {}
                        Err(error) => {
                            eprintln!("Stream Subscriber Error: {}", error);
                        }
                    };
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
    pub fn subscribe_mpsc_sender(&self, subscriber: futures::sync::mpsc::Sender<ReceiveType>) {
        self.subscribers.lock().unwrap().push(subscriber);
    }

    /// callback will be called for each frame polled from the internal stream.
    pub fn on_receive<F>(&self, callback: F)
    where
        F: FnMut(ReceiveType) -> Result<(), ()> + std::marker::Send + 'static,
        ReceiveType: std::marker::Send + 'static,
    {
        let (tx, rx) = channel::<ReceiveType>(10);
        let on_frame = rx.for_each(callback).map(|_| ());
        tokio::spawn(on_frame);
        self.subscribe_mpsc_sender(tx);
    }

    /// Returns an AsyncWriter that will forward data to the associated tokio sink.
    pub fn get_writer(&self) -> AsyncWriter<SendType> {
        AsyncWriter {
            tx: RefCell::new(self.tx.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
