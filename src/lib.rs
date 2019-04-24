extern crate tokio;

use futures::sync::mpsc::Sender;
use futures::{
    future::Future,
    sink::Sink,
    stream::{SplitSink, SplitStream, Stream},
    sync::mpsc::channel,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::{
    codec::{Decoder, Encoder, Framed},
    io::{AsyncRead, AsyncWrite},
};

/// A simple interface to interact with a tokio sink.
///
/// Should always be constructed by a call to some IoManager's get_writer().
#[derive(Clone)]
pub struct IoWriter<Codec>
where
    Codec: Encoder,
{
    tx: futures::sync::mpsc::Sender<<Codec as Encoder>::Item>,
}

impl<Codec> IoWriter<Codec>
where
    Codec: Encoder,
{
    /// Forwards the frame to the tokio sink associated with the IoManager that build this instance.
    pub fn write(
        &mut self,
        frame: <Codec as Encoder>::Item,
    ) -> Result<
        futures::AsyncSink<<Codec as Encoder>::Item>,
        futures::sync::mpsc::SendError<<Codec as Encoder>::Item>,
    > {
        self.tx.start_send(frame)
    }
}

/// A simplified interface to interact with tokio's streams and sinks.
///
/// Allows easy subscription to the stream's frames, and easy sending to the sink.
pub struct IoManager<Codec>
where
    Codec: Encoder + Decoder,
{
    tx: futures::sync::mpsc::Sender<<Codec as Encoder>::Item>,
    subscribers: Arc<Mutex<HashMap<u32, futures::sync::mpsc::Sender<<Codec as Decoder>::Item>>>>,
    next_handle: Mutex<u32>,
}

impl<Codec> IoManager<Codec>
where
    Codec: Decoder + Encoder + std::marker::Send + 'static,
    <Codec as Encoder>::Item: std::marker::Send,
    <Codec as Encoder>::Error: std::marker::Send,
    <Codec as Decoder>::Item: std::marker::Send + Clone,
    <Codec as Decoder>::Error: std::marker::Send,
{
    /// SHOULD ALWAYS BE CALLED FROM INSIDE A TOKIO RUNTIME!
    ///
    /// Builds a new `IoManager` from the provided `sink` and `stream` with no filter.
    ///
    /// You can provide a filter to run on each `frame` before sending said frames to callbacks.
    /// To provide a filter, use `with_filter(sink, stream, callback)`.
    pub fn new<Io>(
        sink: SplitSink<Framed<Io, Codec>>,
        stream: SplitStream<Framed<Io, Codec>>,
    ) -> Self
    where
        Io: AsyncRead + AsyncWrite + std::marker::Send + 'static,
    {
        Self::constructor(
            sink,
            stream,
            None::<
                (fn(
                    <Codec as Decoder>::Item,
                    &mut IoWriter<Codec>,
                ) -> Option<<Codec as Decoder>::Item>),
            >,
        )
    }

    /// SHOULD ALWAYS BE CALLED FROM INSIDE A TOKIO RUNTIME!
    ///
    /// Builds a new IoManager from the provided sink and stream.
    /// You can provide a filter to run on each frame before sending said frames to callbacks.
    ///
    /// Callbacks will not be called if the filter returned None, so if you intend on only having a single callback,
    /// using `filter=callback` with a callback that always returns `None` will save you the cost of the multiple
    /// callbacks handling provided by the `subscibe(callback)` API
    pub fn with_filter<Io, F>(
        sink: SplitSink<Framed<Io, Codec>>,
        stream: SplitStream<Framed<Io, Codec>>,
        filter: F,
    ) -> Self
    where
        Io: AsyncWrite + AsyncRead + std::marker::Send + 'static,
        F: FnMut(
                <Codec as Decoder>::Item,
                &mut IoWriter<Codec>,
            ) -> Option<<Codec as Decoder>::Item>
            + std::marker::Send
            + 'static,
    {
        Self::constructor(sink, stream, Some(filter))
    }

    fn constructor<Io, F>(
        sink: SplitSink<Framed<Io, Codec>>,
        stream: SplitStream<Framed<Io, Codec>>,
        mut filter: Option<F>,
    ) -> Self
    where
        Io: AsyncWrite + AsyncRead + std::marker::Send + 'static,
        F: FnMut(
                <Codec as Decoder>::Item,
                &mut IoWriter<Codec>,
            ) -> Option<<Codec as Decoder>::Item>
            + std::marker::Send
            + 'static,
    {
        let (sink_tx, sink_rx) = channel::<<Codec as Encoder>::Item>(10);
        let sink_task = sink_rx.forward(sink.sink_map_err(|_| ())).map(|_| ());
        tokio::spawn(sink_task);
        let mut filter_writer = IoWriter {
            tx: sink_tx.clone(),
        };

        let subscribers = Arc::new(Mutex::new(HashMap::<
            u32,
            futures::sync::mpsc::Sender<<Codec as Decoder>::Item>,
        >::new()));
        let stream_subscribers_reference = subscribers.clone();
        let stream_task = stream
            .for_each(move |frame: <Codec as Decoder>::Item| {
                let frame = match &mut filter {
                    None => Some(frame),
                    Some(function) => function(frame, &mut filter_writer),
                };
                match frame {
                    Some(frame) => {
                        for (_handle, tx) in stream_subscribers_reference.lock().unwrap().iter_mut()
                        {
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
        IoManager {
            tx: sink_tx,
            subscribers,
            next_handle: Mutex::new(0),
        }
    }

    /// deprecated: use `on_receive()` instead, unless you NEED an `mpsc::Sender` to be notified.
    ///
    /// `subscriber` will receive any data polled from the internal stream.
    pub fn subscribe_mpsc_sender(
        &self,
        subscriber: futures::sync::mpsc::Sender<<Codec as Decoder>::Item>,
    ) -> u32 {
        let mut map = self.subscribers.lock().unwrap();
        let mut handle_guard = self.next_handle.lock().unwrap();
        let handle = handle_guard.clone();
        *handle_guard += 1;
        map.insert(handle.clone(), subscriber);
        handle
    }

    /// `callback` will be called for each `frame` polled from the internal stream.
    pub fn on_receive<F>(&self, callback: F) -> u32
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
        self.subscribe_mpsc_sender(tx)
    }

    /// Removes the callback with `key`handle. `key` should be a value returned by either
    /// `on_receive()` or `subscribe_mpsc_sender()`.
    ///
    /// Returns the `mpsc::Sender` that used to be notified upon new frames, just in case.
    pub fn extract_callback(&self, key: u32) -> Option<Sender<<Codec as Decoder>::Item>> {
        let mut map = self.subscribers.lock().unwrap();
        map.remove(&key)
    }

    /// Returns an `IoWriter` that will forward data to the associated tokio sink.
    pub fn get_writer(&self) -> IoWriter<Codec> {
        IoWriter {
            tx: self.tx.clone(),
        }
    }
}

/// Inspired by bkwilliams, these aliases will probably stay, but you shouldn't rely too much on them
pub mod silly_aliases {
    pub type DoWhenever<T> = crate::IoManager<T>;
    pub type PushWhenever<T> = crate::IoWriter<T>;
}

/// DEPRECATED: These aliases will be discarded whenever an actual API change happens
pub mod legacy_aliases {
    pub type AsyncReadWriter<T> = crate::IoManager<T>;
    pub type AsyncWriter<T> = crate::IoWriter<T>;
}
