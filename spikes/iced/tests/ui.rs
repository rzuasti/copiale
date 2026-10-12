//! Headless UI test: open a conversation, type in the composer, send.

use fixtures::{Config, Dataset, Sender};
use iced::keyboard::{self, key};
use iced::{Event, Size};
use iced_test::selector::id;
use iced_test::simulator::{self, Simulator};
use spike_iced::app::{App, Message, Options};

fn small_dataset() -> Dataset {
    Dataset::generate(&Config {
        messages: 300,
        conversations: 4,
        ..Config::default()
    })
}

/// Runs `interact` against the current view, then applies its messages.
fn interact(app: &mut App, interact: impl FnOnce(&mut Simulator<'_, Message>)) {
    let messages: Vec<Message> = {
        let mut ui = Simulator::with_size(Default::default(), Size::new(1000., 700.), app.view());
        interact(&mut ui);
        ui.into_messages().collect()
    };
    for message in messages {
        let _ = app.update(message);
    }
}

fn shift_enter() -> Vec<Event> {
    let enter = keyboard::Key::Named(key::Named::Enter);
    vec![
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: enter.clone(),
            modified_key: enter.clone(),
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::SHIFT,
            repeat: false,
            text: None,
        }),
        simulator::release_key(enter),
    ]
}

#[test]
fn sends_a_message_from_the_composer() {
    let (mut app, _) = App::new(small_dataset(), Options::default());
    let target = app.store().dataset().conversations[2].clone();

    // Open another conversation through the sidebar.
    interact(&mut app, |ui| {
        ui.click(target.title.as_str())
            .expect("conversation in sidebar");
    });
    assert_eq!(app.open_conversation_id(), target.id);

    let before = target.messages.len();
    let loaded_before = app.loaded_count();

    interact(&mut app, |ui| {
        ui.click(id("composer")).expect("composer");
        // Enter on an empty composer sends nothing.
        ui.tap_key(key::Named::Enter);
    });
    assert_eq!(app.store().conversation(target.id).messages.len(), before);

    interact(&mut app, |ui| {
        ui.click(id("composer")).expect("composer");
        ui.typewrite("hello 中文");
        ui.simulate(shift_enter());
        ui.typewrite("second line");
        ui.tap_key(key::Named::Enter);
    });

    let conversation = app.store().conversation(target.id);
    assert_eq!(conversation.messages.len(), before + 1);
    let sent = conversation.last_message().unwrap();
    assert_eq!(sent.sender, Sender::Me);
    assert_eq!(sent.body, "hello 中文\nsecond line");
    assert_eq!(app.loaded_count(), loaded_before + 1);
    assert!(app.composer_text().trim().is_empty());
}
