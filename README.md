# A Simplified API to work with Tokio's SplitSink and SplitStream

## Motivation
Although Tokio is extremely powerful, somme of its features have been less than intuitive to me.
So I built this crate to simplify interracting with Tokio in the ways that I usually do:
* Writing to an IO without really wanting to do much with what happens then
* Subscribing one or several callbacks to an IO.

## Usage
This API should only be used from inside a Tokio Runtime: it will try to spawn Tokio Tasks and will thus panic if it's not the case.

### Standard Usage: Multiple Callbacks
```rust
fn tokio_main() {
    let (sink, stream) = LineCodec.framed(tcp_stream).split();
    let io = AsyncReadWriter(sink, stream, None);
    let writer = io.get_writer();
    io.subscribe(move |frame| {
        writer.write(frame);
    });
    io.subscribe(move |frame| {
        println!("{}", frame);
    })
}
```

### Filtering
You can use filters to have your callbacks only be called when the frame matches some criterion.
```rust
fn tokio_main() {
    let (sink, stream) = LineCodec.framed(tcp_stream).split();
    let io = AsyncReadWriter(sink, stream, Some(|frame, writer| {
        if frame.to_ascii_lowercase().contains("hello there") {
            writer.write("General Kenobi!");
            return None;
        }
        Some(frame)
    }));
    let writer = io.get_writer();
    io.subscribe(move |frame| {
        writer.write(frame);
    });
    io.subscribe(move |frame| {
        println!("{}", frame);
    })
}
```

### Single Callback Tip
Every time you use `subscribe(callback)`, you endure the cost of one more futures::sync::mpsc::channel,
and of one frame.clone() per callback call.
It's not a high cost, but if you only have one callback, you can cut these costs by passing your callback
as a filter that always returns `None`.
```rust
fn tokio_main() {
    let (sink, stream) = LineCodec.framed(tcp_stream).split();
    let io = AsyncReadWriter(sink, stream, Some(|frame, writer| {
        writer.write(frame);
        None
    }));
}
```