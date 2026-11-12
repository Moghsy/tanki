use serde::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, CoreError>;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoreError {
    Locked,
    UnsafePath(String),
    SchemaTooNew(i64),
    Invalid(String),
    Other(String),
    ActorStopped,
}
impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Locked => f.write_str(
                "This profile is open in Anki (or another tanki). Close it and try again.",
            ),
            Self::SchemaTooNew(v) => write!(
                f,
                "Collection schema {v} is unsupported; tanki needs updating."
            ),
            Self::UnsafePath(s) | Self::Invalid(s) | Self::Other(s) => f.write_str(s),
            Self::ActorStopped => f.write_str("Collection actor stopped"),
        }
    }
}
impl std::error::Error for CoreError {}
impl From<std::io::Error> for CoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Other(e.to_string())
    }
}
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpChanges {
    pub card: bool,
    pub note: bool,
    pub deck: bool,
    pub tag: bool,
    pub notetype: bool,
    pub config: bool,
    pub deck_config: bool,
    pub mtime: bool,
    pub browser_table: bool,
    pub browser_sidebar: bool,
    pub note_text: bool,
    pub study_queues: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deck {
    pub id: i64,
    pub name: String,
    pub new: u32,
    pub learning: u32,
    pub review: u32,
    pub children: Vec<Deck>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedCard {
    pub id: i64,
    pub intervals: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Queue {
    pub new: u32,
    pub learning: u32,
    pub review: u32,
    pub cards: Vec<QueuedCard>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Rating {
    Again,
    Hard,
    Good,
    Easy,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AudioTag {
    SoundOrVideo(String),
    Tts {
        text: String,
        lang: String,
        voices: Vec<String>,
        speed: f32,
        other_args: Vec<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderedCard {
    pub question: String,
    pub answer: String,
    pub css: String,
    pub question_audio: Vec<AudioTag>,
    pub answer_audio: Vec<AudioTag>,
}
#[derive(Debug, Clone)]
pub enum Command {
    DeckTree,
    Queue {
        limit: u32,
    },
    Render {
        card_id: i64,
    },
    Answer {
        card_id: i64,
        rating: Rating,
        answered_at_millis: i64,
        milliseconds_taken: u32,
    },
    Undo,
    AddNote {
        notetype: String,
        fields: Vec<String>,
    },
    Search {
        query: String,
    },
    Backup,
    GenerateFixture,
}
#[derive(Debug, Clone)]
pub enum Response {
    DeckTree(Deck),
    Queue(Queue),
    Rendered(RenderedCard),
    Changes(OpChanges),
    NoteAdded { id: i64, changes: OpChanges },
    CardIds(Vec<i64>),
    Backup,
}
