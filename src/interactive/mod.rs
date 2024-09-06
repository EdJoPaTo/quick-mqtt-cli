use std::time::{Duration, Instant};

use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::layout::{Alignment, Position, Rect};
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::{crossterm, Frame, Terminal};
use rumqttc::{Client, Connection};

use self::ui::ElementInFocus;
use crate::cli::Broker;
use crate::payload::Payload;

mod clean_retained;
mod details;
mod footer;
mod mqtt_error_widget;
mod mqtt_history;
mod mqtt_thread;
mod topic_overview;
mod ui;

enum Refresh {
    /// Update the TUI
    Update,
    /// Skip the update of the TUI
    Skip,
    /// Quit the TUI and return to the shell
    Quit,
}

#[derive(Clone, Copy)]
enum SearchSelection {
    Before,
    Stay,
    After,
}

#[derive(Clone, Copy)]
enum ScrollDirection {
    Up,
    Down,
}

fn reset_terminal() -> anyhow::Result<()> {
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture,
        crossterm::cursor::Show
    )?;
    Ok(())
}

pub fn show(
    client: Client,
    connection: Connection,
    broker: &Broker,
    subscribe_topic: Vec<String>,
    payload_size_limit: usize,
) -> anyhow::Result<()> {
    let mqtt_thread =
        mqtt_thread::MqttThread::new(client, connection, subscribe_topic, payload_size_limit)?;
    let app = App::new(broker, mqtt_thread);

    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        reset_terminal().unwrap();
        original_hook(panic);
    }));

    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(
        stdout,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture,
        crossterm::cursor::Hide
    )?;
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;

    terminal.clear()?;

    let main_loop_result = main_loop(app, terminal);

    reset_terminal()?;

    main_loop_result
}

fn main_loop<B>(mut app: App, mut terminal: Terminal<B>) -> anyhow::Result<()>
where
    B: Backend,
{
    const INTERVAL: Duration = Duration::from_millis(500);
    const DEBOUNCE: Duration = Duration::from_millis(20); // 50 FPS

    terminal.draw(|frame| app.draw(frame))?;

    let mut last_render = Instant::now();
    let mut debounce: Option<Instant> = None;

    loop {
        let timeout = debounce.map_or(INTERVAL, |start| DEBOUNCE.saturating_sub(start.elapsed()));
        if crossterm::event::poll(timeout)? {
            let refresh = match crossterm::event::read()? {
                Event::Key(key) if !matches!(key.kind, KeyEventKind::Press) => Refresh::Skip,
                Event::Key(key) => app.on_key(key)?,
                Event::Mouse(mouse) => match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        app.on_click(mouse.column, mouse.row)
                    }
                    MouseEventKind::ScrollDown => {
                        app.on_scroll(ScrollDirection::Down, mouse.column, mouse.row)
                    }
                    MouseEventKind::ScrollUp => {
                        app.on_scroll(ScrollDirection::Up, mouse.column, mouse.row)
                    }
                    _ => Refresh::Skip,
                },
                Event::Resize(_, _) => Refresh::Update,
                Event::FocusGained | Event::FocusLost | Event::Paste(_) => Refresh::Skip,
            };
            match refresh {
                Refresh::Quit => return Ok(()),
                Refresh::Skip => {}
                Refresh::Update => {
                    debounce.get_or_insert_with(Instant::now);
                }
            }
        }
        if debounce.map_or_else(
            || last_render.elapsed() > INTERVAL,
            |debounce| debounce.elapsed() > DEBOUNCE,
        ) {
            terminal.draw(|frame| app.draw(frame))?;
            last_render = Instant::now();
            debounce = None;
        }
    }
}

pub struct App {
    details: details::Details,
    focus: ElementInFocus,
    footer: footer::Footer,
    mqtt_thread: mqtt_thread::MqttThread,
    topic_overview: topic_overview::TopicOverview,
}

impl App {
    fn new(broker: &Broker, mqtt_thread: mqtt_thread::MqttThread) -> Self {
        Self {
            details: details::Details::default(),
            focus: ElementInFocus::TopicOverview,
            footer: footer::Footer::new(broker),
            mqtt_thread,
            topic_overview: topic_overview::TopicOverview::default(),
        }
    }

    fn can_switch_to_history_table(&self) -> bool {
        let Some(topic) = self.topic_overview.get_selected() else {
            return false;
        };
        self.mqtt_thread.get_history().get(&topic).is_some()
    }

    fn can_switch_to_payload(&self) -> bool {
        let Some(topic) = self.topic_overview.get_selected() else {
            return false;
        };
        self.mqtt_thread
            .get_history()
            .get(&topic)
            .and_then(|entries| {
                let index = self.details.selected_history_index(entries.len());
                entries.get(index)
            })
            .is_some_and(|entry| {
                matches!(
                    entry.payload,
                    Payload::Binary(_) | Payload::Json(_) | Payload::MessagePack(_)
                )
            })
    }

    /// On current topic with the current history table index
    fn get_selected_payload(&self) -> Option<Payload> {
        let topic = self.topic_overview.get_selected()?;
        self.mqtt_thread
            .get_history()
            .get(&topic)
            .and_then(|entries| {
                let index = self.details.selected_history_index(entries.len());
                entries.get(index)
            })
            .map(|entry| entry.payload.clone())
    }

    #[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
    fn on_key(&mut self, key: KeyEvent) -> anyhow::Result<Refresh> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(Refresh::Quit);
        }

        let update = match &self.focus {
            ElementInFocus::TopicOverview => match key.code {
                KeyCode::Char('q') => return Ok(Refresh::Quit),
                KeyCode::Tab if self.can_switch_to_payload() => {
                    self.focus = ElementInFocus::Payload;
                    true
                }
                KeyCode::Tab | KeyCode::BackTab if self.can_switch_to_history_table() => {
                    self.focus = ElementInFocus::HistoryTable;
                    true
                }
                KeyCode::Char('/') => {
                    self.focus = ElementInFocus::TopicSearch;
                    true
                }
                KeyCode::Esc => self.topic_overview.state.select(vec![]),
                KeyCode::Enter | KeyCode::Char(' ') => self.topic_overview.state.toggle_selected(),
                KeyCode::Down | KeyCode::Char('j') => self.topic_overview.state.key_down(),
                KeyCode::Up | KeyCode::Char('k') => self.topic_overview.state.key_up(),
                KeyCode::Left | KeyCode::Char('h') => self.topic_overview.state.key_left(),
                KeyCode::Right | KeyCode::Char('l') => self.topic_overview.state.key_right(),
                KeyCode::Home => self.topic_overview.state.select_first(),
                KeyCode::End => self.topic_overview.state.select_last(),
                KeyCode::PageUp => {
                    let page_jump = (self.topic_overview.last_area.height / 3) as usize;
                    self.topic_overview.state.scroll_up(page_jump)
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let page_jump = (self.topic_overview.last_area.height / 3) as usize;
                    self.topic_overview.state.scroll_up(page_jump)
                }
                KeyCode::PageDown => {
                    let page_jump = (self.topic_overview.last_area.height / 3) as usize;
                    self.topic_overview.state.scroll_down(page_jump)
                }
                KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let page_jump = (self.topic_overview.last_area.height / 3) as usize;
                    self.topic_overview.state.scroll_down(page_jump)
                }
                KeyCode::Backspace | KeyCode::Delete => {
                    if let Some(topic) = self.topic_overview.get_selected() {
                        self.focus = ElementInFocus::CleanRetainedPopup(topic);
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            },
            ElementInFocus::TopicSearch => match key.code {
                KeyCode::Char(char) => {
                    self.topic_overview.search += &char.to_lowercase().to_string();
                    self.search_select(SearchSelection::Stay);
                    true
                }
                KeyCode::Backspace => {
                    self.topic_overview.search.pop();
                    self.search_select(SearchSelection::Stay);
                    true
                }
                KeyCode::Up => self.search_select(SearchSelection::Before),
                KeyCode::Down => self.search_select(SearchSelection::After),
                KeyCode::Enter => {
                    self.search_select(SearchSelection::After);
                    self.topic_overview.state.close_all();
                    self.open_all_search_matches();
                    true
                }
                KeyCode::Esc => {
                    self.topic_overview.search = String::new();
                    self.focus = ElementInFocus::TopicOverview;
                    true
                }
                KeyCode::PageUp => {
                    let page_jump = (self.topic_overview.last_area.height / 3) as usize;
                    self.topic_overview.state.scroll_up(page_jump)
                }
                KeyCode::PageDown => {
                    let page_jump = (self.topic_overview.last_area.height / 3) as usize;
                    self.topic_overview.state.scroll_down(page_jump)
                }
                KeyCode::Tab => {
                    self.focus = ElementInFocus::TopicOverview;
                    true
                }
                _ => false,
            },
            ElementInFocus::Payload => {
                if key.code == KeyCode::Char('q') {
                    return Ok(Refresh::Quit);
                }
                if matches!(key.code, KeyCode::Tab) && self.can_switch_to_history_table() {
                    self.focus = ElementInFocus::HistoryTable;
                    return Ok(Refresh::Update);
                }
                if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                    self.focus = ElementInFocus::TopicOverview;
                    return Ok(Refresh::Update);
                }
                match self.get_selected_payload() {
                    Some(Payload::Binary(_)) => match key.code {
                        KeyCode::Esc => self.details.payload.binary_state.select_address(None),
                        KeyCode::Down | KeyCode::Char('j') => {
                            self.details.payload.binary_state.key_down()
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            self.details.payload.binary_state.key_up()
                        }
                        KeyCode::Left | KeyCode::Char('h') => {
                            self.details.payload.binary_state.key_left()
                        }
                        KeyCode::Right | KeyCode::Char('l') => {
                            self.details.payload.binary_state.key_right()
                        }
                        KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            self.details.payload.binary_state.select_address(Some(0))
                        }
                        KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => self
                            .details
                            .payload
                            .binary_state
                            .select_address(Some(usize::MAX)),
                        KeyCode::Home => self.details.payload.binary_state.select_first_in_row(),
                        KeyCode::End => self.details.payload.binary_state.select_last_in_row(),
                        KeyCode::PageUp => self.details.payload.binary_state.scroll_up(3),
                        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            self.details.payload.binary_state.scroll_up(3)
                        }
                        KeyCode::PageDown => self.details.payload.binary_state.scroll_down(3),
                        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            self.details.payload.binary_state.scroll_down(3)
                        }
                        _ => false,
                    },
                    Some(Payload::Json(_) | Payload::MessagePack(_)) => match key.code {
                        KeyCode::Esc => self.details.payload.json_state.select(vec![]),
                        KeyCode::Enter | KeyCode::Char(' ') => {
                            self.details.payload.json_state.toggle_selected()
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            self.details.payload.json_state.key_down()
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            self.details.payload.json_state.key_up()
                        }
                        KeyCode::Left | KeyCode::Char('h') => {
                            self.details.payload.json_state.key_left()
                        }
                        KeyCode::Right | KeyCode::Char('l') => {
                            self.details.payload.json_state.key_right()
                        }
                        KeyCode::Home => self.details.payload.json_state.select_first(),
                        KeyCode::End => self.details.payload.json_state.select_last(),
                        KeyCode::PageUp => self.details.payload.json_state.scroll_up(3),
                        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            self.details.payload.json_state.scroll_up(3)
                        }
                        KeyCode::PageDown => self.details.payload.json_state.scroll_down(3),
                        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            self.details.payload.json_state.scroll_down(3)
                        }
                        _ => false,
                    },
                    Some(Payload::String(_)) | None => false,
                }
            }
            ElementInFocus::HistoryTable => match key.code {
                KeyCode::Char('q') => return Ok(Refresh::Quit),
                KeyCode::BackTab if self.can_switch_to_payload() => {
                    self.focus = ElementInFocus::Payload;
                    true
                }
                KeyCode::Tab | KeyCode::BackTab => {
                    self.focus = ElementInFocus::TopicOverview;
                    true
                }
                KeyCode::Esc => {
                    let selection = self.details.table_state.selected_mut();
                    let before = *selection;
                    *selection = None;
                    before != *selection
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let selection = self.details.table_state.selected_mut();
                    let before = *selection;
                    *selection = Some(selection.map_or(0, |selection| selection.saturating_add(1)));
                    before != *selection
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    let selection = self.details.table_state.selected_mut();
                    let before = *selection;
                    *selection =
                        Some(selection.map_or(usize::MAX, |selection| selection.saturating_sub(1)));
                    before != *selection
                }
                KeyCode::Home => {
                    let selection = self.details.table_state.selected_mut();
                    let before = *selection;
                    *selection = Some(0);
                    before != *selection
                }
                KeyCode::End => {
                    let selection = self.details.table_state.selected_mut();
                    let before = *selection;
                    *selection = Some(usize::MAX);
                    before != *selection
                }
                KeyCode::PageUp => {
                    let offset = self.details.table_state.offset_mut();
                    let before = *offset;
                    *offset = offset.saturating_sub(3);
                    before != *offset
                }
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let offset = self.details.table_state.offset_mut();
                    let before = *offset;
                    *offset = offset.saturating_sub(3);
                    before != *offset
                }
                KeyCode::PageDown => {
                    let offset = self.details.table_state.offset_mut();
                    *offset = offset.saturating_add(3);
                    true
                }
                KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let offset = self.details.table_state.offset_mut();
                    *offset = offset.saturating_add(3);
                    true
                }
                _ => false,
            },
            ElementInFocus::CleanRetainedPopup(topic) => {
                if matches!(key.code, KeyCode::Enter | KeyCode::Char(' ')) {
                    self.mqtt_thread.clean_below(topic)?;
                }
                self.focus = ElementInFocus::TopicOverview;
                true
            }
        };
        Ok(if update {
            Refresh::Update
        } else {
            Refresh::Skip
        })
    }

    fn on_scroll(&mut self, direction: ScrollDirection, column: u16, row: u16) -> Refresh {
        let position = Position { x: column, y: row };

        let changed = if self.topic_overview.last_area.contains(position) {
            match direction {
                ScrollDirection::Up => self.topic_overview.state.scroll_up(1),
                ScrollDirection::Down => self.topic_overview.state.scroll_down(1),
            }
        } else if self.details.payload.last_area.contains(position) {
            match self.get_selected_payload() {
                Some(Payload::Binary(_)) => {
                    let state = &mut self.details.payload.binary_state;
                    match direction {
                        ScrollDirection::Up => state.scroll_up(1),
                        ScrollDirection::Down => state.scroll_down(1),
                    }
                }
                Some(Payload::Json(_) | Payload::MessagePack(_)) => {
                    let state = &mut self.details.payload.json_state;
                    match direction {
                        ScrollDirection::Up => state.scroll_up(1),
                        ScrollDirection::Down => state.scroll_down(1),
                    }
                }
                Some(Payload::String(_)) | None => return Refresh::Skip,
            }
        } else if self.details.last_table_area.contains(position) {
            let offset = self.details.table_state.offset_mut();
            let before = *offset;
            match direction {
                ScrollDirection::Down => *offset = offset.saturating_add(1),
                ScrollDirection::Up => *offset = offset.saturating_sub(1),
            }
            *offset != before
        } else {
            false
        };
        if changed {
            Refresh::Update
        } else {
            Refresh::Skip
        }
    }

    fn on_click(&mut self, column: u16, row: u16) -> Refresh {
        let position = Position::new(column, row);

        if let Some(identifier) = self.topic_overview.state.rendered_at(position) {
            let is_already_selected = identifier == self.topic_overview.state.selected();
            if is_already_selected {
                // change focus or toggle, don't do both
                if matches!(self.focus, ElementInFocus::TopicOverview) {
                    self.topic_overview.state.toggle_selected();
                } else {
                    self.focus = ElementInFocus::TopicOverview;
                }
            } else {
                self.focus = ElementInFocus::TopicOverview;
                self.topic_overview.state.select(identifier.to_vec());
            }

            return Refresh::Update;
        }
        if self.topic_overview.state.click_at(position) {
            self.focus = ElementInFocus::TopicOverview;
            return Refresh::Update;
        }

        if self.details.payload.last_area.contains(position) {
            match self.get_selected_payload() {
                None => return Refresh::Update, // No payload but click into payload area -> redraw
                Some(Payload::Binary(_)) => {
                    self.details.payload.binary_state.select_at(column, row);
                    self.focus = ElementInFocus::Payload;
                    return Refresh::Update;
                }
                Some(Payload::Json(_) | Payload::MessagePack(_)) => {
                    self.details.payload.json_state.click_at(position);
                    self.focus = ElementInFocus::Payload;
                    return Refresh::Update;
                }
                Some(Payload::String(_)) => return Refresh::Skip,
            }
        }

        if self.details.table_click(position) {
            self.focus = ElementInFocus::HistoryTable;
            return Refresh::Update;
        }

        Refresh::Skip
    }

    // Returns `true` when selection changed
    fn search_select(&mut self, advance: SearchSelection) -> bool {
        let selection = self.topic_overview.get_selected();
        let history = self.mqtt_thread.get_history();
        let mut topics = history
            .get_all_topics()
            .into_iter()
            .enumerate()
            .collect::<Vec<_>>();

        let begin_index = selection
            .and_then(|selection| {
                topics
                    .iter()
                    .find(|(_, topic)| *topic == &selection)
                    .map(|(index, _)| *index)
            })
            .unwrap_or(0);

        // Filter out topics not matching the search
        topics.retain(|(_, topic)| topic.to_lowercase().contains(&self.topic_overview.search));

        let select = match advance {
            SearchSelection::Before => topics
                .iter()
                .rev()
                .find(|(index, _)| *index < begin_index)
                .or_else(|| topics.last()),
            SearchSelection::Stay => topics
                .iter()
                .find(|(index, _)| *index >= begin_index)
                .or_else(|| topics.first()),
            SearchSelection::After => topics
                .iter()
                .find(|(index, _)| *index > begin_index)
                .or_else(|| topics.first()),
        };
        let select = select.map_or(Vec::new(), |(_, topic)| {
            topic.split('/').map(ToOwned::to_owned).collect()
        });
        drop(history);

        for i in 0..select.len() {
            self.topic_overview.state.open(select[0..i].to_vec());
        }

        self.topic_overview.state.select(select)
    }

    fn open_all_search_matches(&mut self) {
        let topics = self
            .mqtt_thread
            .get_history()
            .get_all_topics()
            .into_iter()
            .filter(|topic| topic.to_lowercase().contains(&self.topic_overview.search))
            .map(|topic| topic.split('/').map(ToOwned::to_owned).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        for splitted in topics {
            for i in 0..splitted.len() {
                self.topic_overview.state.open(splitted[0..i].to_vec());
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        const HEADER_HEIGHT: u16 = 1;
        const FOOTER_HEIGHT: u16 = 1;

        let connection_error = self.mqtt_thread.has_connection_err();

        let area = frame.area();
        let Rect { width, height, .. } = area;
        debug_assert_eq!(area.x, 0, "area should fill the whole space");
        debug_assert_eq!(area.y, 0, "area should fill the whole space");

        let header_area = Rect {
            height: HEADER_HEIGHT,
            y: 0,
            ..area
        };
        let footer_area = Rect {
            height: FOOTER_HEIGHT,
            y: height - 1,
            ..area
        };
        let error_height = if connection_error.is_some() { 4 } else { 0 };
        let error_area = Rect {
            height: error_height,
            y: height
                .saturating_sub(FOOTER_HEIGHT)
                .saturating_sub(error_height),
            ..area
        };
        let main_area = Rect {
            height: height
                .saturating_sub(HEADER_HEIGHT + FOOTER_HEIGHT)
                .saturating_sub(error_height),
            y: HEADER_HEIGHT,
            ..area
        };

        if let Some(topic) = self.topic_overview.get_selected() {
            let paragraph = Paragraph::new(Span::styled(topic, ui::STYLE_BOLD));
            frame.render_widget(paragraph.alignment(Alignment::Center), header_area);
        }

        self.footer.draw(frame, footer_area, self);
        if let Some(connection_error) = connection_error {
            mqtt_error_widget::draw(
                frame,
                error_area,
                "MQTT Connection Error",
                &connection_error,
            );
        }

        let history = self.mqtt_thread.get_history();

        let overview_area = self
            .topic_overview
            .get_selected()
            .as_ref()
            .and_then(|selected_topic| history.get(selected_topic))
            .map_or(main_area, |topic_history| {
                let x = width / 3;
                let details_area = Rect {
                    width: width - x,
                    x,
                    ..main_area
                };

                self.details
                    .draw(frame, details_area, topic_history, &self.focus);

                Rect {
                    width: x,
                    x: 0,
                    ..main_area
                }
            });

        self.topic_overview.draw(
            frame,
            overview_area,
            &history,
            matches!(self.focus, ElementInFocus::TopicOverview),
        );
        drop(history);

        if let ElementInFocus::CleanRetainedPopup(topic) = &self.focus {
            clean_retained::draw_popup(frame, topic);
        }
    }
}
