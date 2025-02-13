use iced::{
    Alignment, Color, Element, Length, Sandbox, Settings,
};
use iced::widget::{
    button, column, scrollable, text, text_input, Button, Column, Row, Scrollable, Text,
};

use rusqlite::{Connection, Error as RusqliteError};
use rusqlite::params;
use rusqlite::params_from_iter;
use regex::Regex;
use std::error::Error as StdError;
use std::fs;

/// -------------------------------
/// Custom Text Styles
/// -------------------------------

#[derive(Debug, Clone, Copy)]
struct NormalText;

impl iced::widget::text::StyleSheet for NormalText {
    type Style = iced::Theme;
    fn appearance(&self, _style: Self::Style) -> iced::widget::text::Appearance {
        iced::widget::text::Appearance {
            color: Some(Color::BLACK),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct HighlightText;

impl iced::widget::text::StyleSheet for HighlightText {
    type Style = iced::Theme;
    fn appearance(&self, _style: Self::Style) -> iced::widget::text::Appearance {
        iced::widget::text::Appearance {
            color: Some(Color::from_rgb(1.0, 0.0, 0.0)), // red highlight
            ..Default::default()
        }
    }
}

impl From<NormalText> for iced::theme::Text {
    fn from(_: NormalText) -> Self {
        iced::theme::Text::Color(Color::BLACK)
    }
}

impl From<HighlightText> for iced::theme::Text {
    fn from(_: HighlightText) -> Self {
        iced::theme::Text::Color(Color::from_rgb(1.0, 0.0, 0.0))
    }
}

/// -------------------------------
/// Data Structures and Database Setup
/// -------------------------------

#[derive(Debug)]
struct Verse {
    long_name: String,
    chapter: u32,
    verse: u32,
    text: String,
}

/// (Optional) Register a custom SQL function "regexp" with SQLite.
fn register_regex_function(conn: &Connection) -> Result<(), RusqliteError> {
    use rusqlite::functions::FunctionFlags;
    conn.create_scalar_function(
        "regexp",
        2,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let pattern: String = ctx.get(0)?;
            let text: String = ctx.get(1)?;
            let re = match Regex::new(&pattern) {
                Ok(r) => r,
                Err(e) => return Err(rusqlite::Error::UserFunctionError(Box::new(e))),
            };
            Ok(re.is_match(&text) as i32)
        },
    )
}

/// -------------------------------
/// Helper Functions for Advanced Search & Lookup
/// -------------------------------

/// For advanced search: Build a dynamic WHERE clause from a query (e.g. "faith AND hope").
fn build_where_clause(query: &str) -> (String, Vec<String>) {
    let tokens: Vec<&str> = query.split_whitespace().collect();
    let mut operator = "AND";
    for token in &tokens {
        let upper = token.to_uppercase();
        if upper == "AND" {
            operator = "AND";
            break;
        } else if upper == "OR" {
            operator = "OR";
        }
    }
    let mut conditions = Vec::new();
    let mut params = Vec::new();
    for token in tokens {
        let upper = token.to_uppercase();
        if upper == "AND" || upper == "OR" {
            continue;
        }
        if upper.starts_with("NOT") && token.len() > 3 {
            let term = token[3..].trim();
            if !term.is_empty() {
                conditions.push("text NOT LIKE '%' || ? || '%'".to_string());
                params.push(term.to_string());
            }
        } else {
            conditions.push("text LIKE '%' || ? || '%'".to_string());
            params.push(token.to_string());
        }
    }
    let clause = if conditions.is_empty() {
        "1".to_string()
    } else {
        conditions.join(&format!(" {} ", operator))
    };
    (clause, params)
}

/// For lookup: Parse a lookup reference.
/// Accepts either "Gen 6:1-6" (end chapter omitted, so assume same as start) or "Gen 6:1-7:2".
fn parse_lookup(query: &str) -> Option<(String, u32, u32, u32, u32)> {
    let re = Regex::new(
        r"^(?P<book>\S+)\s+(?P<start_ch>\d+):(?P<start_v>\d+)-(?:(?P<end_ch>\d+):)?(?P<end_v>\d+)$"
    ).ok()?;
    let caps = re.captures(query)?;
    let book = caps.name("book")?.as_str().to_string();
    let start_ch: u32 = caps.name("start_ch")?.as_str().parse().ok()?;
    let start_v: u32 = caps.name("start_v")?.as_str().parse().ok()?;
    let end_ch: u32 = if let Some(m) = caps.name("end_ch") {
        m.as_str().parse().ok()?
    } else {
        start_ch
    };
    let end_v: u32 = caps.name("end_v")?.as_str().parse().ok()?;
    Some((book, start_ch, start_v, end_ch, end_v))
}

/// For highlighting: Split text into segments that match any search token (case‑insensitive).
fn split_for_highlight<'a>(text: &'a str, query: &str) -> Vec<(&'a str, bool)> {
    let tokens: Vec<&str> = query
        .split_whitespace()
        .filter(|&t| {
            let upper = t.to_uppercase();
            upper != "AND" && upper != "OR" && !upper.starts_with("NOT")
        })
        .collect();
    if tokens.is_empty() {
        return vec![(text, false)];
    }
    let pattern = format!("(?i)({})", tokens.join("|"));
    let re = match Regex::new(&pattern) {
        Ok(r) => r,
        Err(_) => return vec![(text, false)],
    };
    let mut segments = Vec::new();
    let mut last_end = 0;
    for mat in re.find_iter(text) {
        let start = mat.start();
        let end = mat.end();
        if start > last_end {
            segments.push((&text[last_end..start], false));
        }
        segments.push((&text[start..end], true));
        last_end = end;
    }
    if last_end < text.len() {
        segments.push((&text[last_end..], false));
    }
    segments
}

/// -------------------------------
/// Application State and Combined UI
/// -------------------------------

struct App {
    // Advanced search state
    search_input: String,
    search_results: Vec<Verse>,
    // Lookup state
    lookup_input: String,
    lookup_results: Vec<Verse>,
    // Compare state: vector of (Bible description, verses) from each Bible database file.
    compare_results: Vec<(String, Vec<Verse>)>,
    // Shared database connection (for advanced search and lookup)
    db: Connection,
}

#[derive(Debug, Clone)]
enum Message {
    // Advanced search messages
    SearchChanged(String),
    SearchSubmitted,
    // Lookup messages
    LookupChanged(String),
    LookupSubmitted,
    // Compare messages
    CompareSubmitted,
}

impl Sandbox for App {
    type Message = Message;

    fn new() -> Self {
        // UPDATE: Replace with the actual path to your main Bible database.
        let db_path = "KJ1769.SQLite3";
        let conn = Connection::open(db_path).expect("Failed to open DB");
        register_regex_function(&conn).expect("Failed to register regex function");
        App {
            search_input: String::new(),
            search_results: Vec::new(),
            lookup_input: String::new(),
            lookup_results: Vec::new(),
            compare_results: Vec::new(),
            db: conn,
        }
    }

    fn title(&self) -> String {
        String::from("Bible Verse Lookup – Search, Lookup & Compare")
    }

    fn update(&mut self, message: Message) {
        match message {
            // Advanced search updates
            Message::SearchChanged(query) => {
                self.search_input = query;
            }
            Message::SearchSubmitted => {
                println!("Advanced Search query: {}", self.search_input);
                let (where_clause, params_vec) = build_where_clause(&self.search_input);
                let sql = format!(
                    "SELECT b.long_name, v.chapter, v.verse, v.text \
                     FROM verses v \
                     JOIN books b ON v.book_number = b.book_number \
                     WHERE {}",
                    where_clause
                );
                println!("Advanced Search SQL Query: {}", sql);
                println!("Advanced Search Parameters: {:?}", params_vec);
                let mut stmt = self.db.prepare(&sql).expect("Failed to prepare statement");
                let verse_iter = stmt
                    .query_map(params_from_iter(params_vec.iter()), |row| {
                        Ok(Verse {
                            long_name: row.get(0)?,
                            chapter: row.get(1)?,
                            verse: row.get(2)?,
                            text: row.get(3)?,
                        })
                    })
                    .expect("Query failed");
                self.search_results = verse_iter.filter_map(|result| result.ok()).collect();
                println!("Advanced Search found {} verses", self.search_results.len());
            }
            // Lookup updates
            Message::LookupChanged(query) => {
                self.lookup_input = query;
            }
            Message::LookupSubmitted => {
                println!("Lookup query: {}", self.lookup_input);
                // When doing a lookup, clear previous compare results.
                self.compare_results.clear();
                if let Some((book, start_ch, start_v, end_ch, end_v)) = parse_lookup(&self.lookup_input) {
                    let sql = "
                        SELECT b.long_name, v.chapter, v.verse, v.text
                        FROM verses v
                        JOIN books b ON v.book_number = b.book_number
                        WHERE b.short_name = ?
                          AND ((v.chapter * 1000) + v.verse) BETWEEN ((? * 1000) + ?) AND ((? * 1000) + ?)
                        ORDER BY v.chapter, v.verse
                    ";
                    println!("Lookup SQL Query: {}", sql);
                    println!("Lookup Parameters: [book: {}, start: {}:{}, end: {}:{}]", book, start_ch, start_v, end_ch, end_v);
                    let mut stmt = self.db.prepare(sql).expect("Failed to prepare statement");
                    let verse_iter = stmt
                        .query_map(params![book, start_ch, start_v, end_ch, end_v], |row| {
                            Ok(Verse {
                                long_name: row.get(0)?,
                                chapter: row.get(1)?,
                                verse: row.get(2)?,
                                text: row.get(3)?,
                            })
                        })
                        .expect("Query failed");
                    self.lookup_results = verse_iter.filter_map(|result| result.ok()).collect();
                    println!("Lookup found {} verses", self.lookup_results.len());
                } else {
                    println!("Failed to parse lookup input: {}", self.lookup_input);
                    self.lookup_results.clear();
                }
            }
            // Compare updates
            Message::CompareSubmitted => {
                println!("Compare lookup based on: {}", self.lookup_input);
                // When doing a comparison, clear previous lookup results.
                self.lookup_results.clear();
                if let Some((book, start_ch, start_v, end_ch, end_v)) = parse_lookup(&self.lookup_input) {
                    self.compare_results.clear();
                    // Look for all files in the current directory with extension ".SQLite3"
                    if let Ok(entries) = fs::read_dir(".") {
                        for entry in entries.filter_map(Result::ok) {
                            let path = entry.path();
                            if let Some(ext) = path.extension() {
                                if ext.to_str().map(|s| s.eq_ignore_ascii_case("SQLite3")).unwrap_or(false) {
                                    if let Ok(bible_conn) = Connection::open(&path) {
                                        // Get the Bible's description from the info table.
                                        let bible_name: String = bible_conn.query_row(
                                            "SELECT value FROM info WHERE name = 'description'",
                                            [],
                                            |row| row.get(0),
                                        ).unwrap_or_else(|_| "Unknown Bible".to_string());
                                        let sql = "
                                            SELECT v.chapter, v.verse, v.text
                                            FROM verses v
                                            JOIN books b ON v.book_number = b.book_number
                                            WHERE b.short_name = ?
                                              AND ((v.chapter * 1000) + v.verse) BETWEEN ((? * 1000) + ?) AND ((? * 1000) + ?)
                                            ORDER BY v.chapter, v.verse
                                        ";
                                        if let Ok(mut stmt) = bible_conn.prepare(sql) {
                                            let verse_iter = stmt
                                                .query_map(params![book, start_ch, start_v, end_ch, end_v], |row| {
                                                    Ok(Verse {
                                                        long_name: bible_name.clone(),
                                                        chapter: row.get(0)?,
                                                        verse: row.get(1)?,
                                                        text: row.get(2)?,
                                                    })
                                                });
                                            if let Ok(iter) = verse_iter {
                                                let verses: Vec<Verse> = iter.filter_map(|v| v.ok()).collect();
                                                println!("Bible '{}' (file {:?}) returned {} verses", bible_name, path, verses.len());
                                                self.compare_results.push((bible_name, verses));
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    println!("Comparison completed with {} Bibles", self.compare_results.len());
                } else {
                    println!("Failed to parse lookup input for compare: {}", self.lookup_input);
                    self.compare_results.clear();
                }
            }
        }
    }

    fn view(&self) -> Element<Message> {
        // Advanced Search Section
        let search_input = text_input("Enter advanced search query...", &self.search_input)
            .on_input(Message::SearchChanged)
            .padding(10);
        let search_button = button(text("Search"))
            .on_press(Message::SearchSubmitted)
            .padding(10);
        let mut search_results_column = Column::new().spacing(10);
        if self.search_results.is_empty() {
            search_results_column = search_results_column.push(text("No advanced search results found").style(NormalText));
        } else {
            search_results_column = search_results_column.push(text(format!("Advanced Search Results ({} verses)", self.search_results.len())).style(NormalText));
            for verse in &self.search_results {
                let header = text(format!("{} {}:{}", verse.long_name, verse.chapter, verse.verse))
                    .size(16)
                    .style(NormalText);
                let segments = split_for_highlight(&verse.text, &self.search_input);
                let mut verse_text_row = Row::new().spacing(0);
                for (segment, is_highlight) in segments {
                    let seg_text = if is_highlight {
                        text(segment).style(HighlightText)
                    } else {
                        text(segment).style(NormalText)
                    };
                    verse_text_row = verse_text_row.push(seg_text);
                }
                search_results_column = search_results_column.push(
                    Column::new().spacing(5).push(header).push(verse_text_row)
                );
            }
        }
        let search_scroll = Scrollable::new(search_results_column).height(Length::Fixed(200.0));
        let advanced_search_section = Column::new()
            .spacing(10)
            .push(search_input)
            .push(search_button)
            .push(search_scroll);

        // Lookup Section
        let lookup_input = text_input("Enter lookup reference (e.g. Gen 6:1-6)...", &self.lookup_input)
            .on_input(Message::LookupChanged)
            .padding(10);
        let lookup_button = button(text("Lookup"))
            .on_press(Message::LookupSubmitted)
            .padding(10);
        let compare_button = button(text("Compare"))
            .on_press(Message::CompareSubmitted)
            .padding(10);
        let mut lookup_results_column = Column::new().spacing(10);
        if self.lookup_results.is_empty() {
            lookup_results_column = lookup_results_column.push(text("No lookup results found").style(NormalText));
        } else {
            lookup_results_column = lookup_results_column.push(text(format!("Lookup Results ({} verses)", self.lookup_results.len())).style(NormalText));
            for verse in &self.lookup_results {
                let header = text(format!("{} {}:{}", verse.long_name, verse.chapter, verse.verse))
                    .size(16)
                    .style(NormalText);
                let verse_text = text(&verse.text).style(NormalText);
                lookup_results_column = lookup_results_column.push(
                    Column::new().spacing(5).push(header).push(verse_text)
                );
            }
        }
        let lookup_scroll = Scrollable::new(lookup_results_column).height(Length::Fixed(200.0));
        let lookup_section = Column::new()
            .spacing(10)
            .push(lookup_input)
            .push(lookup_button)
            .push(compare_button)
            .push(lookup_scroll);

        // Comparison Section
        let compare_header = text(format!("Comparison Results ({} Bibles)", self.compare_results.len()))
            .size(16)
            .style(NormalText);
        let mut compare_results_column = Column::new().spacing(10).push(compare_header);
        if self.compare_results.is_empty() {
            compare_results_column = compare_results_column.push(text("No comparison results found").style(NormalText));
        } else {
            for (bible_name, verses) in &self.compare_results {
                let header = text(format!("Bible: {} ({} verses)", bible_name, verses.len()))
                    .size(16)
                    .style(NormalText);
                let mut bible_column = Column::new().spacing(5).push(header);
                for verse in verses {
                    let verse_line = text(format!("{}:{} {}", verse.chapter, verse.verse, verse.text))
                        .style(NormalText);
                    bible_column = bible_column.push(verse_line);
                }
                compare_results_column = compare_results_column.push(bible_column);
            }
        }
        let compare_scroll = Scrollable::new(compare_results_column).height(Length::Fixed(200.0));
        let comparison_section = Column::new()
            .spacing(10)
            .push(text("Comparison Results").style(NormalText))
            .push(compare_scroll);

        // Combine all sections into one column.
        let content = Column::new()
            .spacing(20)
            .align_items(Alignment::Start)
            .push(advanced_search_section)
            .push(lookup_section)
            .push(comparison_section);

        // Wrap the entire content in a scrollable container.
        Scrollable::new(content).into()
    }
}

fn main() {
    let settings = Settings {
        window: iced::window::Settings {
            size: (800, 600),
            ..Default::default()
        },
        ..Default::default()
    };
    App::run(settings);
}
