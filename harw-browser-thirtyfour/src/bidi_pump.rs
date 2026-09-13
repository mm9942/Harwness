use crate::runtime::FirefoxRuntime;
use futures_util::StreamExt;
use harw_browser::capability::CapabilityStatus;
use harw_browser::event::{BrowserEvent, EventClass, NetworkClass};
use std::sync::Arc;
use thirtyfour::bidi::events::{
    BeforeRequestSent, Load, LogEntryAdded, RealmCreated, RealmDestroyed, ScriptMessage,
};
use tokio::task::JoinHandle;

pub(crate) struct BidiPump {
    tasks: Vec<JoinHandle<()>>,
}

pub(crate) struct BidiPumpStartOutcome {
    pub(crate) pump: Option<BidiPump>,
    pub(crate) browsing_context: CapabilityStatus,
    pub(crate) network: CapabilityStatus,
    pub(crate) log: CapabilityStatus,
    pub(crate) script: CapabilityStatus,
}

impl BidiPump {
    pub(crate) async fn start(runtime: Arc<FirefoxRuntime>) -> BidiPumpStartOutcome {
        let bidi = {
            let driver = runtime.driver().await;
            match driver.webdriver().bidi().await {
                Ok(bidi) => bidi,
                Err(error) => {
                    tracing::warn!(%error, "WebDriver BiDi connection unavailable");
                    return unavailable();
                }
            }
        };
        let mut tasks = Vec::new();

        // Subscribe before bootstrapping existing realms so lifecycle events raised while
        // `get_realms` is in flight are buffered by the typed BiDi event stream.  Console
        // and preload-script consumers are deliberately started only after this point: a
        // realm id is never accepted as a browser-context id.
        match bidi.subscribe::<RealmCreated>().await {
            Ok(mut stream) => {
                let runtime = Arc::clone(&runtime);
                tasks.push(tokio::spawn(async move {
                    while let Some(event) = stream.next().await {
                        register_realm(
                            runtime.as_ref(),
                            event.0.realm.to_string(),
                            event.0.context.map(|context| context.to_string()),
                        )
                        .await;
                    }
                }));
            }
            Err(error) => {
                tracing::warn!(%error, "BiDi script realm-created subscription unavailable; future realm events will degrade");
            }
        }

        match bidi.subscribe::<RealmDestroyed>().await {
            Ok(mut stream) => {
                let runtime = Arc::clone(&runtime);
                tasks.push(tokio::spawn(async move {
                    while let Some(event) = stream.next().await {
                        let realm = event.realm.to_string();
                        if let Err(error) = runtime.remove_bidi_realm(realm.as_str()).await {
                            tracing::warn!(%error, realm, "degraded BiDi script realm removal");
                        }
                    }
                }));
            }
            Err(error) => {
                tracing::warn!(%error, "BiDi script realm-destroyed subscription unavailable");
            }
        }

        match bidi.script().get_realms().await {
            Ok(realms) => {
                for realm in realms.realms {
                    register_realm(
                        runtime.as_ref(),
                        realm.realm.to_string(),
                        realm.context.map(|context| context.to_string()),
                    )
                    .await;
                }
            }
            Err(error) => {
                tracing::warn!(%error, "BiDi script realm bootstrap unavailable; console and script events without a lifecycle mapping will degrade");
            }
        }

        let browsing_context = match bidi.subscribe::<Load>().await {
            Ok(mut stream) => {
                let runtime = Arc::clone(&runtime);
                tasks.push(tokio::spawn(async move { while let Some(event) = stream.next().await {
                    let id = event.context.to_string();
                    match runtime.resolve_bidi_context(&id).await {
                        Ok(context_id) => { let _ = runtime.append_event(EventClass::Critical, BrowserEvent::Navigation { context_id, url: event.url, title: None }).await; }
                        Err(error) => tracing::warn!(%error, bidi_context = id, "degraded unresolved BiDi load event"),
                    }
                }}));
                CapabilityStatus::Native
            }
            Err(error) => {
                tracing::warn!(%error, "BiDi browsing-context subscription unavailable");
                CapabilityStatus::Unavailable
            }
        };
        let network = match bidi.subscribe::<BeforeRequestSent>().await {
            Ok(mut stream) => {
                let runtime = Arc::clone(&runtime);
                tasks.push(tokio::spawn(async move { while let Some(event) = stream.next().await {
                let Some(source) = event.context.map(|id| id.to_string()) else { tracing::warn!("degraded contextless BiDi network event"); continue; };
                match runtime.resolve_bidi_context(&source).await {
                    Ok(context_id) => { let _ = runtime.append_event(EventClass::RequestLifecycle, BrowserEvent::NetworkRequest { context_id, request_id: event.request.id.to_string(), method: event.request.method, url: event.request.url, class: NetworkClass::Unknown }).await; }
                    Err(error) => tracing::warn!(%error, bidi_context = source, "degraded unresolved BiDi network event"),
                }
            }}));
                CapabilityStatus::Native
            }
            Err(error) => {
                tracing::warn!(%error, "BiDi network subscription unavailable");
                CapabilityStatus::Unavailable
            }
        };
        let log = match bidi.subscribe::<LogEntryAdded>().await {
            Ok(mut stream) => {
                let runtime = Arc::clone(&runtime);
                tasks.push(tokio::spawn(async move {
                    while let Some(event) = stream.next().await {
                        let Some(realm) = event.realm.map(|id| id.to_string()) else {
                            tracing::warn!("degraded realm-less BiDi log event");
                            continue;
                        };
                        match runtime.resolve_bidi_realm(&realm).await {
                            Ok(context_id) => {
                                let _ = runtime
                                    .append_event(
                                        EventClass::ConsoleRepetition,
                                        BrowserEvent::ConsoleEntry {
                                            context_id,
                                            level: format!("{:?}", event.level),
                                            message: event.text.unwrap_or_default(),
                                        },
                                    )
                                    .await;
                            }
                            Err(error) => {
                                tracing::warn!(%error, realm, "degraded unresolved BiDi log event")
                            }
                        }
                    }
                }));
                CapabilityStatus::Native
            }
            Err(error) => {
                tracing::warn!(%error, "BiDi log subscription unavailable");
                CapabilityStatus::Unavailable
            }
        };
        let script = match bidi.subscribe::<ScriptMessage>().await {
            Ok(mut stream) => {
                let runtime = Arc::clone(&runtime);
                tasks.push(tokio::spawn(async move { while let Some(event) = stream.next().await {
                let Some(realm) = event.source.get("realm").and_then(|value| value.as_str()) else { tracing::warn!("degraded source-less BiDi script event"); continue; };
                match runtime.resolve_bidi_realm(realm).await {
                    Ok(context_id) => { let _ = runtime.append_event(EventClass::Critical, BrowserEvent::ScriptMessage { context_id, channel: event.channel.to_string(), payload: event.data }).await; }
                    Err(error) => tracing::warn!(%error, realm, "degraded unresolved BiDi script event"),
                }
            }}));
                CapabilityStatus::Native
            }
            Err(error) => {
                tracing::warn!(%error, "BiDi script subscription unavailable");
                CapabilityStatus::Unavailable
            }
        };
        let pump = (!tasks.is_empty()).then_some(BidiPump { tasks });
        BidiPumpStartOutcome {
            pump,
            browsing_context,
            network,
            log,
            script,
        }
    }
}

impl Drop for BidiPump {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

fn unavailable() -> BidiPumpStartOutcome {
    BidiPumpStartOutcome {
        pump: None,
        browsing_context: CapabilityStatus::Unavailable,
        network: CapabilityStatus::Unavailable,
        log: CapabilityStatus::Unavailable,
        script: CapabilityStatus::Unavailable,
    }
}

async fn register_realm(runtime: &FirefoxRuntime, realm: String, bidi_context: Option<String>) {
    let Some(bidi_context) = bidi_context else {
        tracing::warn!(realm, "degraded contextless BiDi script realm");
        return;
    };

    let context_id = match runtime.resolve_bidi_context(&bidi_context).await {
        Ok(context_id) => context_id,
        Err(error) => {
            tracing::warn!(%error, realm, bidi_context, "degraded unresolved BiDi script realm");
            return;
        }
    };

    if let Err(error) = runtime.register_bidi_realm(realm.clone(), context_id).await {
        tracing::warn!(%error, realm, bidi_context, "degraded rejected BiDi script realm registration");
    }
}
