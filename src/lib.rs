extern crate futures_promises;
extern crate tokio;

use futures::sync::mpsc::Sender;
use futures::{future::Future, sink::Sink, stream::Stream, sync::mpsc::channel};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_promises::promises::{Promise, PromiseHandle};

#[cfg(not(any(feature = "big_channels", feature = "very_big_channels")))]
const CHANNEL_SIZE: usize = 16;
#[cfg(all(feature = "big_channels", not(feature = "very_big_channels")))]
const CHANNEL_SIZE: usize = 64;
#[cfg(feature = "very_big_channels")]
const CHANNEL_SIZE: usize = 256;

/// A simple interface to interact with a tokio sink.
///
/// Should always be constructed by a call to some IoManager's get_writer().
pub struct IoWriter<SendType> {
    tx: futures::sync::mpsc::Sender<SendType>,
}

impl<SendType> Clone for IoWriter<SendType> {
    fn clone(&self) -> Self {
        IoWriter {
            tx: self.tx.clone(),
        }
    }
}

impl<SendType> IoWriter<SendType>
where
    SendType: std::marker::Send + 'static,
{
    pub fn new<SinkType>(sink: SinkType) -> Self
    where
        SinkType: Sink<SinkItem = SendType> + Send + 'static,
    {
        let (tx, sink_rx) = channel::<<SinkType as Sink>::SinkItem>(CHANNEL_SIZE);
        let sink_task = sink_rx.forward(sink.sink_map_err(|_| ())).map(|_| ());
        tokio::spawn(sink_task);
        IoWriter { tx }
    }

    /// Forwards the frame to the tokio sink associated with the IoManager that build this instance.
    pub fn write<T: Into<SendType>>(&mut self, frame: T) -> PromiseHandle<()> {
        let promise = Promise::new();
        let handle = promise.get_handle();
        tokio::spawn(self.tx.clone().send(frame.into()).then(move |result| {
            match result {
                Ok(_) => promise.resolve(()),
                Err(e) => {
                    promise.reject(format!("{}", e));
                }
            };
            Ok::<(), ()>(())
        }));
        handle
    }
}

pub trait Filter<SendType, ReceiveType>:
    FnMut(ReceiveType, &mut IoWriter<SendType>) -> Option<ReceiveType> + std::marker::Send + 'static
{
}

impl<T, SendType, ReceiveType> Filter<SendType, ReceiveType> for T where
    T: FnMut(ReceiveType, &mut IoWriter<SendType>) -> Option<ReceiveType>
        + std::marker::Send
        + 'static
{
}

pub trait ErrorHandler<ErrorType>: FnMut(ErrorType) + std::marker::Send + 'static {}

impl<T, ErrorType> ErrorHandler<ErrorType> for T where
    T: FnMut(ErrorType) + std::marker::Send + 'static
{
}

/// A builder for `IoManager`, and the only way to build one since the constructors have been deleted.
pub struct IoManagerBuilder<
    SinkType,
    StreamType,
    BF = (fn(
        <StreamType as Stream>::Item,
        &mut IoWriter<<SinkType as Sink>::SinkItem>,
    ) -> Option<<StreamType as Stream>::Item>),
    BEH = (fn(<StreamType as Stream>::Error)),
> where
    SinkType: Sink,
    StreamType: Stream,
    BF: FnMut(
            <StreamType as Stream>::Item,
            &mut IoWriter<<SinkType as Sink>::SinkItem>,
        ) -> Option<<StreamType as Stream>::Item>
        + std::marker::Send
        + 'static,
    BEH: FnMut(<StreamType as Stream>::Error) + std::marker::Send + 'static,
{
    sink: SinkType,
    stream: StreamType,
    filter: Option<BF>,
    error_handler: Option<BEH>,
}

type DefaultFilterType<SinkType, StreamType> = (fn(
    <StreamType as Stream>::Item,
    &mut IoWriter<<SinkType as Sink>::SinkItem>,
) -> Option<<StreamType as Stream>::Item>);
type DefaultErrorHandlerType<StreamType> = (fn(<StreamType as Stream>::Error));

impl<SinkType, StreamType> IoManagerBuilder<SinkType, StreamType>
where
    SinkType: Sink + Send + 'static,
    StreamType: Stream + Send + 'static,
    <StreamType as Stream>::Item: Send + Clone + 'static,
    <StreamType as Stream>::Error: Send,
    <SinkType as Sink>::SinkItem: Send + 'static,
{
    /// Creates a builder for `IoManager`.
    pub fn new(
        sink: SinkType,
        stream: StreamType,
    ) -> IoManagerBuilder<
        SinkType,
        StreamType,
        DefaultFilterType<SinkType, StreamType>,
        DefaultErrorHandlerType<StreamType>,
    > {
        IoManagerBuilder {
            sink,
            stream,
            filter: None,
            error_handler: None,
        }
    }
}

impl<SinkType, StreamType, FilterType, ErrorHandlerType>
    IoManagerBuilder<SinkType, StreamType, FilterType, ErrorHandlerType>
where
    SinkType: Sink + Send + 'static,
    StreamType: Stream + Send + 'static,
    <StreamType as Stream>::Item: Send + Clone + 'static,
    <StreamType as Stream>::Error: Send,
    <SinkType as Sink>::SinkItem: Send + 'static,
    FilterType: Filter<<SinkType as Sink>::SinkItem, <StreamType as Stream>::Item>,
    ErrorHandlerType: ErrorHandler<<StreamType as Stream>::Error>,
{
    /// Adds a filter to the `IoManager` builder.
    /// Filters are static in this library. If you need to be able to change the filter without
    /// droping the sink and steram passed to this instance, you should probably use Box to encapsulate your filter,
    /// and then whatever you need to make it all thread safe for when you'll need to modify it.
    /// Type inference should still work, which is nice.
    pub fn with_filter<NewFilterType>(
        self,
        filter: NewFilterType,
    ) -> IoManagerBuilder<SinkType, StreamType, NewFilterType, ErrorHandlerType>
    where
        NewFilterType: Filter<<SinkType as Sink>::SinkItem, <StreamType as Stream>::Item>,
    {
        IoManagerBuilder {
            sink: self.sink,
            stream: self.stream,
            filter: Some(filter),
            error_handler: self.error_handler,
        }
    }

    /// Similar to `with_filter`, only for error handling.
    /// Tip: if you want to be able to catch end of streams with this API,
    /// you may want your Codec to implement `decode_eof()` and throw an error at the last moment,
    /// such as this:
    /// ```rust
    /// fn decode_eof(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
    ///     let decode = self.decode(buf);
    ///     match decode {
    ///         Ok(None) => Err(std::io::Error::from(std::io::ErrorKind::ConnectionAborted)),
    ///         _ => decode,
    ///     }
    /// }
    /// ```
    pub fn with_error_handler<NewErrorHandlerType>(
        self,
        handler: NewErrorHandlerType,
    ) -> IoManagerBuilder<SinkType, StreamType, FilterType, NewErrorHandlerType>
    where
        NewErrorHandlerType: ErrorHandler<<StreamType as Stream>::Error>,
    {
        IoManagerBuilder {
            sink: self.sink,
            stream: self.stream,
            filter: self.filter,
            error_handler: Some(handler),
        }
    }

    pub fn build(self) -> IoManager<SinkType::SinkItem, StreamType::Item> {
        IoManager::<SinkType::SinkItem, StreamType::Item>::constructor(
            self.sink,
            self.stream,
            self.filter,
            self.error_handler,
        )
    }
}

/// A simplified interface to interact with tokio's streams and sinks.
///
/// Allows easy subscription to the stream's frames, and easy sending to the sink.
#[derive(Clone)]
pub struct IoManager<SendType, ReceiveType = SendType> {
    tx: futures::sync::mpsc::Sender<SendType>,
    subscribers: Arc<Mutex<HashMap<u32, futures::sync::mpsc::Sender<ReceiveType>>>>,
    next_handle: Arc<Mutex<u32>>,
}

impl<SendType, ReceiveType> IoManager<SendType, ReceiveType> {
    fn constructor<SinkType, StreamType, F, EH>(
        sink: SinkType,
        stream: StreamType,
        mut filter: Option<F>,
        error_handler: Option<EH>,
    ) -> IoManager<SinkType::SinkItem, StreamType::Item>
    where
        SinkType: Sink + Send + 'static,
        StreamType: Stream + Send + 'static,
        <StreamType as Stream>::Item: Send + Clone + 'static,
        <StreamType as Stream>::Error: Send,
        <SinkType as Sink>::SinkItem: Send + 'static,
        F: Filter<SinkType::SinkItem, StreamType::Item>,
        EH: ErrorHandler<StreamType::Error>,
    {
        let (sink_tx, sink_rx) = channel::<<SinkType as Sink>::SinkItem>(CHANNEL_SIZE);
        let sink_task = sink_rx.forward(sink.sink_map_err(|_| ())).map(|_| ());
        tokio::spawn(sink_task);
        let mut filter_writer = IoWriter {
            tx: sink_tx.clone(),
        };

        let subscribers = Arc::new(Mutex::new(HashMap::<
            u32,
            futures::sync::mpsc::Sender<<StreamType as Stream>::Item>,
        >::new()));
        let stream_subscribers_reference = subscribers.clone();
        let stream_task = stream
            .for_each(move |frame| {
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
            .map_err(|e| match error_handler {
                None => (),
                Some(mut handler) => handler(e),
            });
        tokio::spawn(stream_task);
        IoManager {
            tx: sink_tx,
            subscribers,
            next_handle: Arc::new(Mutex::new(0)),
        }
    }

    /// `subscriber` will receive any data polled from the internal stream.
    pub fn subscribe_mpsc_sender(
        &self,
        subscriber: futures::sync::mpsc::Sender<ReceiveType>,
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
        F: FnMut(ReceiveType) -> Result<(), ()> + std::marker::Send + 'static,
        ReceiveType: std::marker::Send + 'static,
    {
        let (tx, rx) = channel::<ReceiveType>(CHANNEL_SIZE);
        let on_frame = rx.for_each(callback).map(|_| ());
        tokio::spawn(on_frame);
        self.subscribe_mpsc_sender(tx)
    }

    /// Removes the callback with `key`handle. `key` should be a value returned by either
    /// `on_receive()` or `subscribe_mpsc_sender()`.
    ///
    /// Returns the `mpsc::Sender` that used to be notified upon new frames, just in case.
    pub fn extract_callback(&self, key: &u32) -> Option<Sender<ReceiveType>> {
        let mut map = self.subscribers.lock().unwrap();
        map.remove(key)
    }

    /// Returns an `IoWriter` that will forward data to the associated tokio sink.
    pub fn get_writer(&self) -> IoWriter<SendType> {
        IoWriter {
            tx: self.tx.clone(),
        }
    }
}

/// Inspired by bkwilliams, these aliases will probably stay, but you shouldn't rely too much on them
pub mod silly_aliases {
    pub type DoWhenever<T, U> = crate::IoManager<T, U>;
    pub type PushWhenever<T> = crate::IoWriter<T>;
}
