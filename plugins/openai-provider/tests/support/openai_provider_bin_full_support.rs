    async fn dispatch_direct_completion(output_schema: Option<Value>) -> Vec<Map<String, Value>> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).await.expect("read request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            let body = "data: {\"choices\":[{\"delta\":{\"reasoning\":\"checking\"}}]}\n\n\
                        data: {\"choices\":[{\"delta\":{\"content\":\"{\\\"content\\\":\"}}]}\n\n\
                        data: {\"choices\":[{\"delta\":{\"content\":\"\\\"ok\\\"}\"}}]}\n\n\
                        data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n\n\
                        data: {\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n\
                        data: [DONE]\n\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        });

        let auth = Arc::new(AuthStore::from_env_key(Some("test-key".into())));
        let chats = fresh_chats("test-model");
        let completions = Arc::new(CompletionRuns::new());
        let catalog = Arc::new(ToolCatalog::new());
        let broker = Arc::new(ToolBroker::new());
        let mut config = cfg("ollama");
        config.base_url = format!("http://{addr}");
        let client = reqwest::Client::builder().build().expect("client");
        let (tx, mut rx) = mpsc::channel(16);
        let mut body = make_event_body(
            "ollama.completion.request",
            &[
                ("request_id", Value::String("req-direct".into())),
                (
                    "messages",
                    serde_json::json!([{"role": "user", "content": "answer"}]),
                ),
            ],
        );
        if let Some(schema) = output_schema {
            body.insert("output_schema".into(), schema);
        }

        dispatch_event_with_completions(
            &chats,
            &completions,
            &auth,
            &catalog,
            &broker,
            &config,
            &client,
            &tx,
            &from_plugin("mag"),
            &body,
        )
        .await
        .expect("dispatch ok");

        let mut emitted = Vec::new();
        loop {
            let outgoing = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .expect("completion event timeout")
                .expect("completion event channel closed");
            let value: Value =
                serde_json::from_str(&outgoing.to_line()).expect("plugin outgoing json");
            let event = value
                .get("body")
                .and_then(Value::as_object)
                .expect("event body")
                .clone();
            let terminal = matches!(
                event.get("event").and_then(Value::as_str),
                Some("completed" | "error")
            );
            emitted.push(event);
            if terminal {
                break;
            }
        }
        server.await.expect("server task");
        emitted
    }
