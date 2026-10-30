use crate::{
    auth::AuthUser,
    error::AppResult,
    state::AppState,
};
use axum::{
    Json,
    extract::{Query, State},
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
pub struct ActivityQuery {
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub period: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

pub async fn index(
    State(state): State<AppState>,
    auth: AuthUser,
    Query(query): Query<ActivityQuery>,
) -> AppResult<Json<Value>> {
    auth.require("activity.view")?;
    let limit = query.limit.unwrap_or(100).clamp(1, 500);

    let mut where_sql = String::from(" WHERE 1=1");
    let mut binds: Vec<String> = vec![];

    if let Some(q) = query.q.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        where_sql.push_str(" AND (a.summary LIKE ? OR IFNULL(a.actor,'') LIKE ? OR a.action LIKE ? OR IFNULL(u.name,'') LIKE ?)");
        let like = format!("%{q}%");
        binds.push(like.clone());
        binds.push(like.clone());
        binds.push(like.clone());
        binds.push(like);
    }

    // Compare against host-local calendar (SQLite ignores offsets in stored ISO strings).
    match query.period.as_deref() {
        Some("today") => {
            where_sql.push_str(" AND date(a.created_at) = date('now', 'localtime')")
        }
        Some("week") => {
            where_sql
                .push_str(" AND datetime(a.created_at) >= datetime('now', 'localtime', '-7 days')")
        }
        Some("month") => {
            where_sql
                .push_str(" AND datetime(a.created_at) >= datetime('now', 'localtime', '-30 days')")
        }
        _ => {}
    }

    if let Some(kind) = query.kind.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        if kind.contains('.') {
            where_sql.push_str(" AND a.action = ?");
            binds.push(kind.to_string());
        } else {
            where_sql.push_str(" AND (a.action = ? OR a.action LIKE ?)");
            binds.push(kind.to_string());
            binds.push(format!("{kind}.%"));
        }
    }

    let count_sql = format!(
        "SELECT COUNT(*) FROM activity_logs a LEFT JOIN users u ON u.id = a.user_id{where_sql}"
    );
    let mut count_q = sqlx::query_scalar::<_, i64>(&count_sql);
    for b in &binds {
        count_q = count_q.bind(b);
    }
    let total = count_q.fetch_one(&state.pool).await?;

    let list_sql = format!(
        "SELECT a.id, a.action, a.summary, COALESCE(a.actor, u.name), u.role, u.title,
                a.user_id, a.branch_id, a.metadata_json, a.created_at
         FROM activity_logs a
         LEFT JOIN users u ON u.id = a.user_id
         {where_sql}
         ORDER BY a.created_at DESC
         LIMIT {limit}"
    );
    let mut list_q = sqlx::query_as::<
        _,
        (
            i64,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<String>,
            String,
        ),
    >(&list_sql);
    for b in &binds {
        list_q = list_q.bind(b);
    }
    let rows = list_q.fetch_all(&state.pool).await?;

    let activity_logs: Vec<Value> = rows
        .into_iter()
        .map(
            |(
                id,
                action,
                summary,
                actor,
                actor_role,
                actor_title,
                user_id,
                branch_id,
                metadata_json,
                created_at,
            )| {
                let metadata: Value = metadata_json
                    .as_deref()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or(Value::Null);
                json!({
                    "id": id,
                    "action": action,
                    "summary": summary,
                    "actor": actor,
                    "actor_role": actor_role,
                    "actor_title": actor_title,
                    "user_id": user_id,
                    "branch_id": branch_id,
                    "metadata": metadata,
                    "created_at": created_at,
                })
            },
        )
        .collect();

    Ok(Json(json!({
        "activity_logs": activity_logs,
        "total": total,
    })))
}
