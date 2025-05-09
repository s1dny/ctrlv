use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use chrono::{DateTime, Utc};
use rand::seq::SliceRandom;
use serde::Deserialize;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::net::TcpListener;
use tower_http::services::ServeDir;

struct AppState {
    pastes: Mutex<HashMap<String, Paste>>,
    wordlist: Vec<String>,
}

struct Paste {
    content: String,
    created_at: Instant,
    expires_at: Instant,
}

#[derive(Deserialize)]
struct PasteForm {
    content: String,
}

#[tokio::main]
async fn main() {
    // initialize tracing
    tracing_subscriber::fmt::init();

    // read the wordlist
    let wordlist_path = PathBuf::from("wordlist.txt");
    let file = File::open(&wordlist_path).expect("Failed to open wordlist.txt");
    let reader = BufReader::new(file);
    let wordlist: Vec<String> = reader.lines().filter_map(Result::ok).collect();

    tracing::info!("Loaded {} words from wordlist", wordlist.len());

    // create app state
    let app_state = Arc::new(AppState {
        pastes: Mutex::new(HashMap::new()),
        wordlist,
    });

    // build our application with routes
    let app = Router::new()
        .route("/", get(home_handler))
        .route("/paste", post(create_paste_handler))
        .route("/:id", get(view_paste_handler))
        .route("/:id/raw", get(raw_paste_handler))
        .nest_service("/static", ServeDir::new("static"))
        .with_state(app_state);

    // run the server
    let listener = TcpListener::bind("0.0.0.0:3000").await.unwrap();
    tracing::info!("Listening on http://localhost:3000");
    axum::serve(listener, app).await.unwrap();
}

// handler for the home page
async fn home_handler() -> Html<String> {
    Html(include_str!("../static/index.html").to_string())
}

// handler for creating a new paste
async fn create_paste_handler(
    State(state): State<Arc<AppState>>,
    Form(form): Form<PasteForm>,
) -> impl IntoResponse {
    // generate a unique id
    let id = generate_unique_id(&state);

    // store the paste with a 24-hour ttl
    let now = Instant::now();
    let expires_at = now + Duration::from_secs(24 * 60 * 60);
    let paste = Paste {
        content: form.content,
        created_at: now,
        expires_at,
    };

    // add to the pastes hashmap
    {
        let mut pastes = state.pastes.lock().unwrap();
        pastes.insert(id.clone(), paste);
    }

    // clean up expired pastes
    cleanup_expired_pastes(&state);

    // redirect to the paste view
    Redirect::to(&format!("/{}", id))
}

// handler for viewing a paste
async fn view_paste_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match get_paste(&state, &id) {
        Some(paste) => {
            let creation_time_str = format_datetime(paste.created_at);
            let paste_size_str = format_size(paste.content.len());

            let html = include_str!("../static/view.html")
                .replace("{{PASTE_ID}}", &id)
                .replace("{{PASTE_CONTENT}}", &html_escape(&paste.content))
                .replace("{{CREATION_TIME}}", &creation_time_str)
                .replace("{{PASTE_SIZE}}", &paste_size_str);
            Html(html).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            String::from("paste not found or expired"),
        )
            .into_response(),
    }
}

// handler for viewing raw paste content
async fn raw_paste_handler(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    match get_paste(&state, &id) {
        Some(paste) => (
            StatusCode::OK,
            [("Content-Type", "text/plain; charset=utf-8")],
            paste.content,
        )
            .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            [("Content-Type", "text/plain; charset=utf-8")],
            "paste not found or expired.".to_string(),
        )
            .into_response(),
    }
}

// helper function to get a paste if it exists and hasn't expired
fn get_paste(state: &Arc<AppState>, id: &str) -> Option<Paste> {
    let pastes = state.pastes.lock().unwrap();
    pastes.get(id).and_then(|paste_ref| {
        if paste_ref.expires_at > Instant::now() {
            Some(Paste {
                content: paste_ref.content.clone(),
                created_at: paste_ref.created_at,
                expires_at: paste_ref.expires_at,
            })
        } else {
            None
        }
    })
}

// helper function to generate a unique id from two random words
fn generate_unique_id(state: &Arc<AppState>) -> String {
    let mut rng = rand::thread_rng();

    loop {
        let word1 = state.wordlist.choose(&mut rng).unwrap();
        let word2 = state.wordlist.choose(&mut rng).unwrap();
        let id = format!("{}.{}", word1, word2);

        // ensure the id is unique
        let pastes = state.pastes.lock().unwrap();
        if !pastes.contains_key(&id) {
            return id;
        }
    }
}

// helper function to clean up expired pastes
fn cleanup_expired_pastes(state: &Arc<AppState>) {
    let now = Instant::now();
    let mut pastes = state.pastes.lock().unwrap();

    // collect keys to remove
    let expired_keys: Vec<String> = pastes
        .iter()
        .filter(|(_, paste)| paste.expires_at <= now)
        .map(|(id, _)| id.clone())
        .collect();

    // remove expired pastes
    for key in expired_keys {
        pastes.remove(&key);
    }
}

// helper function to escape html characters
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

// helper function to format an instant into a human-readable datetime string
fn format_datetime(instant: Instant) -> String {
    let elapsed = Instant::now().duration_since(instant);

    let system_time = SystemTime::now().checked_sub(elapsed).unwrap_or(UNIX_EPOCH);

    let datetime: DateTime<Utc> = system_time.into();

    datetime.format("%Y-%m-%d %H:%M:%S").to_string()
}

// helper function to format a size in bytes into a human-readable string
fn format_size(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;

    let size = bytes as f64;

    if size < KB {
        format!("{:.1}B", size)
    } else if size < MB {
        format!("{:.1}KB", size / KB)
    } else if size < GB {
        format!("{:.1}MB", size / MB)
    } else {
        format!("{:.1}GB", size / GB)
    }
}
