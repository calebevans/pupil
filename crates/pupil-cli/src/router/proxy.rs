use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

use super::{RoutingEngine, SessionAffinityMap};

pub struct RouterState {
    pub engine: Arc<RoutingEngine>,
    pub affinity: Arc<SessionAffinityMap>,
    pub http_client: reqwest::Client,
}

#[derive(Debug, serde::Deserialize)]
pub struct ChatCompletionRequest {
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream: bool,
    #[serde(flatten)]
    pub extra: serde_json::Value,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

pub async fn handle_chat_completion(
    State(state): State<Arc<RouterState>>,
    headers: HeaderMap,
    axum::Json(request): axum::Json<ChatCompletionRequest>,
) -> impl IntoResponse {
    let user_message = request
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.content.as_str())
        .unwrap_or("");

    if user_message.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({
                "error": "No user message found in request"
            })),
        )
            .into_response();
    }

    let session_id = extract_session_id(&headers);
    let reroute = headers
        .get("x-pupil-reroute")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    let target_agent = if reroute {
        if let Some(ref sid) = session_id {
            state.affinity.remove(sid);
        }
        None
    } else {
        session_id
            .as_ref()
            .and_then(|sid| state.affinity.get(sid))
            .and_then(|name| {
                state
                    .engine
                    .registry
                    .get_agent(&name)
                    .filter(|a| a.is_healthy())
                    .map(|_| name)
            })
    };

    let (agent_name, _decision) = match target_agent {
        Some(name) => {
            metrics::counter!("pupil_router_session_affinity_hits_total").increment(1);
            (name, None)
        }
        None => match state.engine.route(user_message).await {
            Ok(decision) => {
                if decision.agent.is_empty() {
                    return handle_no_match(&state.engine, &decision).into_response();
                }

                metrics::counter!(
                    "pupil_router_queries_total",
                    "agent" => decision.agent.clone(),
                    "tier" => decision.strategy_tier.to_string(),
                )
                .increment(1);

                if decision.strategy_tier == super::StrategyTier::Fallback {
                    metrics::counter!("pupil_router_fallback_total").increment(1);
                }

                let name = decision.agent.clone();
                (name, Some(decision))
            }
            Err(e) => {
                tracing::error!(error = %e, "Routing failed");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    axum::Json(serde_json::json!({
                        "error": "routing_failed",
                        "message": e.to_string()
                    })),
                )
                    .into_response();
            }
        },
    };

    if let Some(ref sid) = session_id {
        state.affinity.set(sid, &agent_name);
    }

    let agent = match state.engine.registry.get_agent(&agent_name) {
        Some(a) => a,
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({
                    "error": "agent_not_found",
                    "message": format!("Agent '{}' not found in registry", agent_name)
                })),
            )
                .into_response();
        }
    };

    let target_url = format!("{}/v1/chat/completions", agent.url);

    let forward_body = serde_json::json!({
        "messages": request.messages,
        "stream": request.stream,
    });
    let mut body_map = match forward_body {
        serde_json::Value::Object(m) => m,
        _ => unreachable!(),
    };
    if let serde_json::Value::Object(extra) = request.extra {
        for (k, v) in extra {
            body_map.entry(k).or_insert(v);
        }
    }

    let proxy_start = std::time::Instant::now();
    let proxy_result = state.http_client.post(&target_url).json(&body_map).send().await;

    match proxy_result {
        Ok(resp) => {
            let elapsed = proxy_start.elapsed().as_secs_f64();
            metrics::histogram!(
                "pupil_router_proxy_duration_seconds",
                "agent" => agent_name.clone()
            )
            .record(elapsed);

            let status = resp.status();
            let mut response_headers = HeaderMap::new();
            response_headers.insert(
                "x-pupil-routed-to",
                agent_name
                    .parse()
                    .unwrap_or_else(|_| axum::http::HeaderValue::from_static("unknown")),
            );

            if let Some(ct) = resp.headers().get("content-type") {
                response_headers.insert("content-type", ct.clone());
            }

            let body = Body::from_stream(resp.bytes_stream());

            let mut response = Response::new(body);
            *response.status_mut() = status;
            *response.headers_mut() = response_headers;

            response.into_response()
        }
        Err(e) => {
            tracing::error!(
                agent = %agent_name,
                url = %target_url,
                error = %e,
                "Proxy request failed"
            );
            (
                StatusCode::BAD_GATEWAY,
                axum::Json(serde_json::json!({
                    "error": "proxy_failed",
                    "message": format!("Failed to reach agent '{}': {}", agent_name, e)
                })),
            )
                .into_response()
        }
    }
}

fn handle_no_match(
    engine: &RoutingEngine,
    decision: &super::RoutingDecision,
) -> impl IntoResponse {
    let suggestions: Vec<serde_json::Value> = engine
        .registry
        .healthy_agents()
        .iter()
        .map(|a| {
            serde_json::json!({
                "name": a.name,
                "description": a.description,
            })
        })
        .collect();

    (
        StatusCode::OK,
        axum::Json(serde_json::json!({
            "error": "no_matching_agent",
            "message": "No agent matched your query with sufficient confidence.",
            "reasoning": decision.reasoning,
            "suggestions": suggestions,
        })),
    )
}

pub async fn handle_list_agents(
    State(state): State<Arc<RouterState>>,
) -> axum::Json<serde_json::Value> {
    let agents: Vec<serde_json::Value> = state
        .engine
        .registry
        .all_agents()
        .iter()
        .map(|a| {
            serde_json::json!({
                "name": a.name,
                "url": a.url,
                "description": a.description,
                "topics": a.topics,
                "healthy": a.is_healthy(),
                "priority": a.priority,
            })
        })
        .collect();

    axum::Json(serde_json::json!({ "agents": agents }))
}

pub async fn handle_get_agent(
    State(state): State<Arc<RouterState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> impl IntoResponse {
    match state.engine.registry.get_agent(&name) {
        Some(agent) => axum::Json(serde_json::json!({
            "name": agent.name,
            "url": agent.url,
            "description": agent.description,
            "topics": agent.topics,
            "exclusive_topics": agent.exclusive_topics,
            "sample_questions": agent.sample_questions,
            "healthy": agent.is_healthy(),
            "priority": agent.priority,
        }))
        .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({
                "error": "agent_not_found",
                "message": format!("Agent '{}' not found", name)
            })),
        )
            .into_response(),
    }
}

pub async fn handle_health(
    State(state): State<Arc<RouterState>>,
) -> axum::Json<serde_json::Value> {
    let all = state.engine.registry.all_agents();
    let healthy_count = all.iter().filter(|a| a.is_healthy()).count();

    let agents: Vec<serde_json::Value> = all
        .iter()
        .map(|a| {
            serde_json::json!({
                "name": a.name,
                "healthy": a.is_healthy(),
            })
        })
        .collect();

    axum::Json(serde_json::json!({
        "status": if healthy_count > 0 { "healthy" } else { "degraded" },
        "agents_total": all.len(),
        "agents_healthy": healthy_count,
        "agents": agents,
    }))
}

fn extract_session_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get("x-session-id")
        .or_else(|| headers.get("x-request-id"))
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}
