//! ACP agent server implementation.

use std::sync::Arc;

use agent_client_protocol::schema::v1::{
    AgentCapabilities, CancelNotification, CloseSessionRequest, Implementation, InitializeRequest,
    InitializeResponse, ListSessionsRequest, LoadSessionRequest, NewSessionRequest, PromptRequest,
    ResumeSessionRequest, SessionCapabilities, SessionCloseCapabilities, SessionListCapabilities,
    SessionResumeCapabilities, SetSessionModeRequest,
};
use agent_client_protocol::{Agent, Stdio};
use pacode_client::ClientOptions;
use pacode_types::Config;

use crate::error::AcpError;
use crate::session::SessionManager;

/// Runs the ACP agent server over stdio until EOF or disconnect.
pub async fn run_server(client_opts: ClientOptions, _config: Config) -> Result<(), AcpError> {
    let session_mgr = Arc::new(SessionManager::new(client_opts));

    let mgr_new = Arc::clone(&session_mgr);
    let mgr_load = Arc::clone(&session_mgr);
    let mgr_resume = Arc::clone(&session_mgr);
    let mgr_prompt = Arc::clone(&session_mgr);
    let mgr_set_mode = Arc::clone(&session_mgr);
    let mgr_list = Arc::clone(&session_mgr);
    let mgr_close = Arc::clone(&session_mgr);
    let mgr_cancel = Arc::clone(&session_mgr);

    Agent
        .builder()
        .name("pacode")
        .on_receive_request(
            async move |init: InitializeRequest, responder, _cx| {
                let response = InitializeResponse::new(init.protocol_version)
                    .agent_capabilities(
                        AgentCapabilities::new()
                            .load_session(true)
                            .session_capabilities(
                                SessionCapabilities::new()
                                    .resume(SessionResumeCapabilities::new())
                                    .list(SessionListCapabilities::new())
                                    .close(SessionCloseCapabilities::new()),
                            ),
                    )
                    .agent_info(
                        Implementation::new("pacode", pacode_config::APP_VERSION).title("pacode"),
                    );
                responder.respond(response)
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: NewSessionRequest, responder, _cx| match mgr_new.new_session(req).await
            {
                Ok(resp) => responder.respond(resp),
                Err(e) => responder.respond_with_internal_error(e.to_string()),
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: LoadSessionRequest, responder, cx| match mgr_load
                .load_session(req, &cx)
                .await
            {
                Ok(resp) => responder.respond(resp),
                Err(e) => responder.respond_with_internal_error(e.to_string()),
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: ResumeSessionRequest, responder, cx| match mgr_resume
                .resume_session(req, &cx)
                .await
            {
                Ok(resp) => responder.respond(resp),
                Err(e) => responder.respond_with_internal_error(e.to_string()),
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: PromptRequest, responder, cx| {
                let session_mgr = Arc::clone(&mgr_prompt);
                let cx_clone = cx.clone();
                cx.spawn(async move {
                    session_mgr.handle_prompt(req, responder, cx_clone).await;
                    Ok(())
                })
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: SetSessionModeRequest, responder, _cx| match mgr_set_mode
                .set_mode(req)
                .await
            {
                Ok(resp) => responder.respond(resp),
                Err(e) => responder.respond_with_internal_error(e.to_string()),
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: ListSessionsRequest, responder, _cx| match mgr_list
                .list_sessions(req)
                .await
            {
                Ok(resp) => responder.respond(resp),
                Err(e) => responder.respond_with_internal_error(e.to_string()),
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: CloseSessionRequest, responder, _cx| match mgr_close
                .close_session(req)
                .await
            {
                Ok(resp) => responder.respond(resp),
                Err(e) => responder.respond_with_internal_error(e.to_string()),
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |notif: CancelNotification, _cx| {
                mgr_cancel.handle_cancel(notif).await;
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_to(Stdio::new())
        .await
        .map_err(AcpError::Protocol)
}
