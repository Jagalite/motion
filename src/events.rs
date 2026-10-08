use crate::{App, api::ApiError};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::sse::{Event, KeepAlive, Sse},
};
use serde::Deserialize;
use std::{convert::Infallible, time::Duration};
use utoipa::IntoParams;
#[derive(Deserialize, IntoParams)]
pub struct Cursor {
    pub after: Option<i64>,
}
#[utoipa::path(operation_id="subscribe_events",get,path="/api/v1/events",params(Cursor,("Last-Event-ID"=Option<String>,Header)),responses((status=200,description="SSE change hints with persistent IDs; reset means refetch state",content_type="text/event-stream"),(status=400,description="Invalid cursor",body=crate::api::ErrorBody),(status=503,description="Subscriber capacity exhausted",body=crate::api::ErrorBody)))]
pub async fn subscribe(
    State(app): State<App>,
    Query(q): Query<Cursor>,
    headers: HeaderMap,
) -> Result<Sse<impl futures_core::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let after = if let Some(h) = headers.get("last-event-id") {
        Some(
            h.to_str()
                .ok()
                .and_then(|s| s.parse::<i64>().ok())
                .ok_or_else(|| ApiError::bad("Invalid event cursor"))?,
        )
    } else {
        q.after
    };
    if after.is_some_and(|n| n < 0) {
        return Err(ApiError::bad("Invalid event cursor"));
    }
    let permit = app.event_streams.clone().try_acquire_owned().map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "event_limit",
            "Too many event subscribers",
        )
    })?;
    let latest: i64 = sqlx::query_scalar("SELECT coalesce(max(id),0) FROM change_events")
        .fetch_one(&app.db)
        .await?;
    let stream = async_stream::stream! {
        let _permit=permit;let mut cursor=after.unwrap_or(latest);
        if after.is_none(){yield Ok(Event::default().event("reset").id(cursor.to_string()).data("{\"reason\":\"initial_snapshot\"}"));}
        loop {
            if app.health.shutting_down.load(std::sync::atomic::Ordering::Relaxed){break;}
            let batch:Result<_,sqlx::Error>=async{
                let mut tx=app.db.begin().await?;
                let (min,max):(i64,i64)=sqlx::query_as("SELECT coalesce(min(id),0),coalesce(max(id),0) FROM change_events").fetch_one(&mut *tx).await?;
                let rows:Vec<(i64,String,String)>=sqlx::query_as("SELECT id,topic,resource_id FROM change_events WHERE id>? ORDER BY id LIMIT 100").bind(cursor).fetch_all(&mut *tx).await?;
                tx.commit().await?;Ok((min,max,rows))
            }.await;
            let Ok((min,max,rows))=batch else {break;};
            if let Some(reset)=playscale_core::events::reset_cursor(cursor,min,max) {cursor=reset;yield Ok(Event::default().event("reset").id(cursor.to_string()).data("{\"reason\":\"cursor_expired\"}"));}
            else {for(id,topic,resource)in rows{cursor=id;yield Ok(Event::default().event("change").id(id.to_string()).data(serde_json::json!({"topic":topic,"resource_id":resource}).to_string()));}}
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}
