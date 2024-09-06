use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

pub fn draw_popup(frame: &mut Frame, topic: &str) {
    let block = Block::bordered()
        .border_style(Style::new().fg(Color::Red))
        .title_alignment(Alignment::Center)
        .title("Clean retained topics");
    let text = vec![
        Line::raw("Clean the following topic and all relative below?"),
        Line::styled(
            topic,
            Style::new().add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ),
        Line::raw(""),
        Line::raw("Confirm with Enter, abort with Esc"),
    ];
    let text = Text::from(text);
    let area = popup_area(frame.area(), text.width());
    let paragraph = Paragraph::new(text)
        .block(block)
        .alignment(Alignment::Center);
    frame.render_widget(Clear, area); // clear the background of the popup
    frame.render_widget(paragraph, area);
}

/// helper function to create a centered area using up certain percentage of the available `area`.
fn popup_area(area: Rect, text_width: usize) -> Rect {
    let height = area.height.min(6);
    let max_width = area.width.saturating_sub(4);
    #[allow(clippy::cast_possible_truncation)]
    let width = text_width.saturating_add(14).min(max_width as usize) as u16;
    Rect {
        x: area.width.saturating_sub(width) / 2,
        y: area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}
