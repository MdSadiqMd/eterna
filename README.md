# Eterna

## Architecture

### Order submission

```mermaid
sequenceDiagram
    participant C  as Client
    participant N  as nginx :80
    participant Ax as api-N :8080
    participant E  as engine :9000
    participant MX as Mutex&lt;OrderBook&gt;
    participant BC as broadcast::Sender&lt;Fill&gt;

    Note over N: resolver 127.0.0.11 valid=5s<br/>set $upstream api:8080<br/>DNS resolved per-request → round-robin across replicas

    C->>+N: POST /orders<br/>{side, price, qty}

    Note over N,Ax: Docker DNS returns one api replica IP.<br/>nginx proxies the request over HTTP/1.1 keep-alive.
    N->>+Ax: POST http://api-N:8080/orders

    Note over Ax,E: api is a stateless proxy.<br/>engine hostname resolved via Docker DNS → engine:9000.
    Ax->>+E: POST http://engine:9000/orders<br/>(reqwest, plain HTTP)

    Note over E,MX: Single serialisation point.<br/>All api instances compete for this lock.<br/>No two orders can enter submit() concurrently.
    E->>+MX: Mutex::lock().await

    MX->>MX: next_id() → id
    MX->>MX: OrderBook::submit(Order{id, side, price, qty})
    Note over MX: BTreeMap&lt;Reverse&lt;u64&gt;, VecDeque&lt;Order&gt;&gt;<br/>Free-list pool — no malloc on level create/drain<br/>Price-time priority matching loop
    MX-->>-E: (id, fills: Vec&lt;Fill&gt;)

    Note over E,BC: Mutex dropped here — before any async work.<br/>Lock is never held across an .await point.

    loop for each fill
        E->>BC: broadcast::Sender::send(fill)
        Note over BC: tokio broadcast channel (capacity 1024).<br/>Each subscribed api gets its own Receiver.
    end

    E-->>-Ax: {"id": N}
    Ax-->>-N: {"id": N}
    N-->>-C: {"id": N}
```

### Fill fan-out — WebSocket

```mermaid
sequenceDiagram
    participant BC as broadcast::Sender&lt;Fill&gt;
    participant A1 as api-1
    participant A2 as api-2
    participant A3 as api-3
    participant W1 as WS clients → api-1
    participant W2 as WS clients → api-2
    participant W3 as WS clients → api-3

    Note over BC,A3: engine sends one fill event; all api instances receive it<br/>regardless of which api processed the original POST /orders.

    BC-)A1: WS text frame  {"maker_order_id":…,"taker_order_id":…,"price":…,"qty":…}
    BC-)A2: WS text frame  (same fill)
    BC-)A3: WS fill frame  (same fill)

    Note over A1,A3: Each api holds a local broadcast::Sender&lt;Fill&gt;.<br/>WS clients connect to GET /ws on any api replica.

    par fan-out
        A1-)W1: fill JSON  (to all clients on api-1)
        A2-)W2: fill JSON  (to all clients on api-2)
        A3-)W3: fill JSON  (to all clients on api-3)
    end
```

## Flamegraphs

See [`flamegraphs/`](flamegraphs/) for profiling results and analysis.
