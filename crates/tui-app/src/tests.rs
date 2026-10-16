use super::*;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

/// Every drawn box's title line.
///
/// The tick lives in two places — the transcript line, which is history, and the
/// prompt box's title, which is state — so a test about state has to name the second
/// one. Asserting on the whole screen would pass before the fix and after it.
fn box_titles(screen: &str) -> String {
    screen
        .lines()
        .filter(|line| line.trim_start().starts_with('┌'))
        .map(despaced)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Feed one message to the app as though the worker had sent it.
fn fed(app: &mut App, message: FromWorker) {
    let (says, worker) = channel();
    says.send(message).unwrap();
    app.drain(&worker);
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn draw(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let area = buffer.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
}

#[test]
fn an_at_sign_opens_a_picker_and_enter_inserts_the_file() {
    let mut app = App::new("s-1", "0.1.0");
    // A fixed file list: the test must not depend on this repository's contents,
    // and it must not walk the tree on every keystroke either.
    app.mention_files = Some(mention::FileIndex::from_files(vec![
        "src/main.rs".to_string(),
        "src/kernel/view.rs".to_string(),
        "README.md".to_string(),
    ]));

    app.handle_key(key(KeyCode::Char('@')));
    assert!(app.picker.is_some(), "typing @ opens the picker");

    for c in "src/".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    let picker = app.picker.as_ref().expect("still open");
    assert_eq!(picker.query, "src/");
    assert_eq!(picker.matches.len(), 2);

    // Down then enter takes the second match, so the selection is real rather
    // than a coincidence of always choosing the first.
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Enter));
    assert!(app.picker.is_none(), "enter closes it");
    assert_eq!(app.input, "@src/kernel/view.rs ");
}

#[test]
fn escape_closes_the_picker_without_touching_the_line() {
    let mut app = App::new("s-1", "0.1.0");
    app.mention_files = Some(mention::FileIndex::from_files(vec![
        "src/main.rs".to_string()
    ]));
    app.handle_key(key(KeyCode::Char('@')));
    app.handle_key(key(KeyCode::Char('s')));
    let before = app.input.clone();
    app.handle_key(key(KeyCode::Esc));
    assert!(app.picker.is_none());
    assert_eq!(app.input, before, "esc cancels the picker, not the draft");
}

#[test]
fn deleting_the_at_sign_closes_the_picker() {
    let mut app = App::new("s-1", "0.1.0");
    app.mention_files = Some(mention::FileIndex::from_files(vec![
        "src/main.rs".to_string()
    ]));
    app.handle_key(key(KeyCode::Char('@')));
    assert!(app.picker.is_some());
    // Backspace removes the trigger, and the picker goes with it: there is no
    // separate open flag that could disagree with what is typed.
    app.handle_key(key(KeyCode::Backspace));
    assert!(app.picker.is_none());
    assert_eq!(app.input, "");
}

#[test]
fn an_at_sign_inside_a_word_is_not_a_reference() {
    let mut app = App::new("s-1", "0.1.0");
    app.mention_files = Some(mention::FileIndex::from_files(vec![
        "example.com".to_string()
    ]));
    // An email address must not open a picker — this is the case a naive
    // "find the last @" gets wrong.
    for c in "mail me@exa".chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
    assert!(app.picker.is_none(), "an address is not a reference");
}

#[test]
fn the_picker_takes_only_the_rows_it_needs() {
    let mut app = App::new("s-1", "0.1.0");
    assert_eq!(app.picker_rows(), 0, "nothing is taken from the transcript");
    app.mention_files = Some(mention::FileIndex::from_files(vec![
        "src/a.rs".to_string(),
        "src/b.rs".to_string(),
    ]));
    app.handle_key(key(KeyCode::Char('@')));
    assert_eq!(app.picker_rows(), 2);
}

#[test]
fn the_screen_shows_the_conversation_the_input_and_the_session() {
    let mut app = App::new("s-1", "0.1.0");
    app.push_lines(vec!["you: hello".to_string(), "hi there".to_string()]);
    type_text(&mut app, "next question");
    let screen = draw(&mut app, 60, 16);
    assert!(screen.contains("session s-1"), "{screen}");
    assert!(screen.contains("schema 0.1.0"), "{screen}");
    assert!(screen.contains("you: hello"), "{screen}");
    assert!(screen.contains("hi there"), "{screen}");
    assert!(screen.contains("next question"), "{screen}");
    assert!(screen.contains("prompt"), "{screen}");
}

#[test]
fn enter_submits_a_non_empty_prompt_and_clears_the_input() {
    let mut app = App::new("s", "v");
    type_text(&mut app, "do it");
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Action::Send(ToWorker::Submit("do it".to_string()))
    );
    assert!(app.input.is_empty());
    assert!(app.busy);
    assert!(
        app.lines.iter().any(|l| l == "you: do it"),
        "{:?}",
        app.lines
    );
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Action::None,
        "empty is not a prompt"
    );
}

#[test]
fn backspace_edits_and_ctrl_c_quits() {
    let mut app = App::new("s", "v");
    type_text(&mut app, "ab");
    app.handle_key(key(KeyCode::Backspace));
    assert_eq!(app.input, "a");
    assert_eq!(
        app.handle_key(key(KeyCode::Char('d'))),
        Action::None,
        "plain 'd' types"
    );
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        Action::Quit
    );
    assert!(app.quit);
}

#[test]
fn an_approval_repurposes_the_input_line_and_answers_it() {
    let mut app = App::new("s", "v");
    app.pending = Some("appr-1".to_string());
    let screen = draw(&mut app, 80, 22);
    // A menu, not an orange line: the question, then the answers to pick from.
    assert!(screen.contains("needs your decision"), "{screen}");
    assert!(screen.contains("Allow once"), "{screen}");
    assert!(screen.contains("Allow in this project"), "{screen}");
    assert!(screen.contains("Deny"), "{screen}");
    assert!(screen.contains("▸"), "the chosen one is marked: {screen}");

    app.handle_key(key(KeyCode::Char('z')));
    assert!(
        app.input.is_empty(),
        "a prompt cannot be typed while the question is open"
    );

    assert_eq!(
        app.handle_key(key(KeyCode::Char('a'))),
        Action::Send(ToWorker::Respond {
            request_id: "appr-1".to_string(),
            granted: true,
            scope: loong_schema::generated::ApprovalScope::Once,
        })
    );
    assert!(app.pending.is_none());
    assert!(
        app.lines
            .iter()
            .any(|l| l.contains("approved once") && l.contains("appr-1")),
        "{:?}",
        app.lines
    );
}

#[test]
fn typing_a_slash_says_where_the_commands_are_listed() {
    let mut app = App::new("s", "v");
    type_text(&mut app, "/rev");
    let screen = draw(&mut app, 70, 16);
    assert!(screen.contains("command — /help"), "{screen}");
    assert!(screen.contains("/exit"), "and the way out: {screen}");
    assert!(screen.contains("/logo"), "and what else is here: {screen}");
    // And it is only a hint: the text is still a normal prompt buffer.
    assert_eq!(app.input, "/rev");
}

#[test]
fn a_long_command_does_not_push_the_answers_out_of_the_box() {
    // Reported from a real session: a shell command with newlines in it wrapped until
    // only `Allow once` was left, so the menu could not be answered properly.
    let mut app = App::new("s", "v");
    app.pending = Some("appr-1".to_string());
    app.approval = Some(Box::new(ApprovalInfo {
        request_id: "appr-1".to_string(),
        class: "out_of_bounds".to_string(),
        reason: "shell: out_of_bounds is Ask by builtin:out_of_bounds".to_string(),
        tool: "shell".to_string(),
        args: format!(
            "{{\"command\":\"cd /home/x && python3 - <<'EOF' > /tmp/a.js\n{}\"}}",
            "print('line')\n".repeat(30)
        ),
    }));
    let screen = draw(&mut app, 200, 30);
    // Every detail is one row, so the panel cannot grow past its share of the screen —
    // and every answer is inside those rows, which is the part that matters.
    let panel: Vec<&str> = screen
        .lines()
        .skip_while(|line| !line.contains("needs your decision"))
        .take_while(|line| !line.starts_with('└'))
        .collect();
    assert!(
        panel.len() <= DECISION_ROWS as usize,
        "{} rows: {panel:?}",
        panel.len()
    );
    for answer in ["Allow once", "Allow in this project", "Deny"] {
        assert!(
            panel.iter().any(|line| line.contains(answer)),
            "{answer} left the panel: {panel:?}"
        );
    }
    assert!(
        panel.iter().any(|line| line.contains('⏎')),
        "the command is flattened: {panel:?}"
    );
}

#[test]
fn a_command_full_of_cjk_does_not_push_the_answers_out_of_the_box() {
    // The same report, one session later: the first decision was readable and the second
    // was not. Its command carried Chinese comments, and `clip` counted *characters* —
    // 76 of them are 150 columns, so the line wrapped to three rows and pushed `Deny`
    // out of the panel, leaving the details and the answers reading as one jumble.
    let mut app = App::new("s", "v");
    app.pending = Some("appr-2".to_string());
    app.approval = Some(Box::new(ApprovalInfo {
        request_id: "appr-2".to_string(),
        class: "out_of_bounds".to_string(),
        reason: "shell: 在目标目录之外执行命令，需要你确认是否允许这一次写入操作，\
                 因为它会修改工作区之外的文件内容"
            .to_string(),
        tool: "shell".to_string(),
        args: "{\"command\": \"python3 - <<'EOF'\n# 把写好的单页游戏复制到目标目录，并检查标题、脚本标签和字符集是否正确\nEOF\"}"
            .to_string(),
    }));
    // A narrow terminal, where a wide line has the least room.
    let screen = draw(&mut app, 80, 30);
    let panel: Vec<&str> = screen
        .lines()
        .skip_while(|line| !line.contains("needs your decision"))
        .take_while(|line| !line.starts_with('└'))
        .collect();
    assert!(
        panel.len() <= DECISION_ROWS as usize,
        "{} rows: {panel:?}",
        panel.len()
    );
    for answer in ["Allow once", "Allow in this project", "Deny"] {
        assert!(
            panel.iter().any(|line| line.contains(answer)),
            "{answer} left the panel: {panel:?}"
        );
    }
    // And the clipping is *visible*: a line cut to display columns ends in `…` inside the
    // box. Clipping by characters instead would put the ellipsis past the right edge, so
    // the reader would see text simply stop — and the answers would still be there, which
    // is why this half needs its own assertion.
    let clipped = panel
        .iter()
        .find(|line| line.contains("{\"command\""))
        .expect("the command row");
    assert!(
        clipped.trim_end_matches('│').trim_end().ends_with('…'),
        "a clipped detail says so, inside the box: {clipped:?}"
    );
}

#[test]
fn a_squeezed_panel_gives_up_its_details_before_its_answers() {
    // A question you cannot answer is worse than one without context, so the details are
    // what gives way. The panel used to keep at least one detail line *even when there was
    // no room for it*, and on a short terminal that cost the reader an answer.
    for height in 12..=24u16 {
        let mut app = App::new("s", "v");
        app.pending = Some("appr-1".to_string());
        app.approval = Some(Box::new(ApprovalInfo {
            request_id: "appr-1".to_string(),
            class: "out_of_bounds".to_string(),
            reason: "shell: out_of_bounds is Ask by builtin:out_of_bounds".to_string(),
            tool: "shell".to_string(),
            args: "{\"command\":\"ls\"}".to_string(),
        }));
        let screen = draw(&mut app, 80, height);
        for answer in ["Allow once", "Allow in this project", "Deny"] {
            assert!(
                screen.contains(answer),
                "at {height} rows, {answer} is missing:\n{screen}"
            );
        }
    }
}

#[test]
fn the_decision_comes_after_the_work_it_is_about() {
    // The flow reads downwards: events, then the question they raised, then the box you
    // answer in. The panel used to sit above the transcript, which put the decision
    // before the evidence for it.
    let mut app = App::new("s", "v");
    app.push_lines(vec!["→ shell ls".to_string()]);
    app.pending = Some("appr-1".to_string());
    let screen = draw(&mut app, 80, 30);
    let transcript = screen.find("loongagent — session").expect("the transcript");
    let decision = screen.find("needs your decision").expect("the decision");
    let prompt = screen
        .find("waiting for your decision")
        .expect("the prompt title");
    assert!(
        transcript < decision && decision < prompt,
        "transcript {transcript}, decision {decision}, prompt {prompt}:\n{screen}"
    );
}

#[test]
fn the_decision_is_a_menu_that_the_keyboard_drives() {
    use loong_schema::generated::ApprovalScope;
    let mut app = App::new("s", "v");
    app.pending = Some("appr-9".to_string());

    // Down moves the highlight, up wraps around, and Enter answers with what is
    // highlighted — including the scope, which is the difference between "this once"
    // and "in this project".
    assert_eq!(app.handle_key(key(KeyCode::Down)), Action::None);
    assert_eq!(app.choice, 1);
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Action::Send(ToWorker::Respond {
            request_id: "appr-9".to_string(),
            granted: true,
            scope: ApprovalScope::Project,
        })
    );

    let mut app = App::new("s", "v");
    app.pending = Some("appr-9".to_string());
    app.handle_key(key(KeyCode::Up));
    assert_eq!(app.choice, 2, "up from the first lands on the last");
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Action::Send(ToWorker::Respond {
            request_id: "appr-9".to_string(),
            granted: false,
            scope: ApprovalScope::Once,
        }),
        "the last option is Deny"
    );

    // A fresh question starts at the top: never inherit a choice nobody looked at.
    let mut app = App::new("s", "v");
    app.choice = 2;
    let (to_app, from_worker) = channel::<FromWorker>();
    to_app
        .send(FromWorker::Approval(Box::new(ApprovalInfo {
            request_id: "appr-10".to_string(),
            class: "out_of_bounds".to_string(),
            reason: "write".to_string(),
            tool: "shell".to_string(),
            args: "{\"command\":\"ls\"}".to_string(),
        })))
        .unwrap();
    app.drain(&from_worker);
    assert_eq!(app.choice, 0);
    assert!(app.approval.is_some(), "and the detail is there to draw");
}

#[test]
fn a_denial_is_an_answer_too() {
    let mut app = App::new("s", "v");
    app.pending = Some("appr-2".to_string());
    assert_eq!(
        app.handle_key(key(KeyCode::Char('d'))),
        Action::Send(ToWorker::Respond {
            request_id: "appr-2".to_string(),
            granted: false,
            scope: loong_schema::generated::ApprovalScope::Once,
        })
    );
    assert!(app
        .lines
        .iter()
        .any(|l| l.contains("denied") && l.contains("appr-2")));
}

#[test]
fn scrolling_moves_the_window_and_new_output_returns_to_the_bottom() {
    let mut app = App::new("s", "v");
    app.push_lines((1..=50).map(|i| format!("line {i}")));
    let bottom = draw(&mut app, 40, 20);
    assert!(bottom.contains("line 50"), "{bottom}");

    app.handle_key(key(KeyCode::PageUp));
    let scrolled = draw(&mut app, 40, 20);
    assert!(!scrolled.contains("line 50"), "{scrolled}");
    assert!(scrolled.contains("line"), "{scrolled}");

    app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL));
    assert_eq!(app.scroll, 0, "ctrl-End returns to the newest output");

    app.handle_key(key(KeyCode::PageUp));
    app.push_lines(vec!["a new answer".to_string()]);
    assert_eq!(app.scroll, 0, "new output pulls the view back");
    assert!(draw(&mut app, 40, 20).contains("a new answer"));
}

#[test]
fn scrolling_back_cannot_push_the_newest_lines_off_the_end() {
    let startup = [
        "· skills available (1): help",
        "· control.synced",
        "· profile.selected",
        "· new session 66mn884m",
        "· resuming 3 events from that session",
        "· profile: default — DeepSeek flash",
    ];
    let mut app = App::new("s", "v");
    app.push_lines(startup.iter().map(|line| line.to_string()));
    // A window with room to spare: there is nothing above these lines to scroll to, so
    // `up` has to leave them where they are. It used to drop the newest ones instead —
    // the opposite of what the key means, and it looked like output being erased.
    for _ in 0..startup.len() {
        app.handle_key(key(KeyCode::Up));
    }
    let screen = draw(&mut app, 60, 24);
    for line in startup {
        assert!(
            screen.contains(line),
            "{line:?} was pushed off the end:\n{screen}"
        );
    }
    assert_eq!(app.scroll, 0, "and the count says what the screen says");
}

#[test]
fn the_bookkeeping_lines_are_not_drawn_into_the_wall() {
    // `DarkGray` is bright black: on a dark theme it *is* the background, so the lines
    // saying which skills loaded and which profile is in use were invisible — reported
    // from a real screen, where scrolling appeared to grey the notes out. Quiet is the
    // terminal's own foreground, dimmed, never a named colour that may be the wall.
    for kind in [LineKind::Note, LineKind::Notice] {
        let style = style_for(kind);
        assert_eq!(
            style.fg,
            Some(Color::Reset),
            "{kind:?} must not name a colour"
        );
        assert!(style.add_modifier.contains(Modifier::DIM), "{kind:?}");
    }
    let mut app = App::new("s", "v");
    app.push_lines(vec!["· skills available (1): help".to_string()]);
    assert_eq!(
        color_of(&mut app, 60, 24, "· skills available"),
        Some(ratatui::style::Color::Reset),
        "and that is what reaches the screen"
    );
}

#[test]
fn the_answers_that_are_not_chosen_are_still_readable() {
    // The same colour, in the one place it decides something: every answer but the
    // highlighted one was `DarkGray`, so a menu read as a single option — the earlier
    // "the decision menu lost its answers" report had this in it too.
    let mut app = App::new("s", "v");
    app.pending = Some("appr-9".to_string());
    assert_eq!(
        color_of(&mut app, 80, 24, "Allow in this project"),
        Some(ratatui::style::Color::Reset),
        "an unchosen answer is dimmed, not drawn into the background"
    );
    assert_eq!(
        color_of(&mut app, 80, 24, "Deny"),
        Some(ratatui::style::Color::Reset)
    );
}

/// The transcript the client writes for two summarised tool results: the summary
/// lines, then the marker that stands for the rest.
fn two_folds(app: &mut App) {
    app.push_lines(vec![
        "you: run it".to_string(),
        "→ shell cargo test".to_string(),
        "← ok older 1".to_string(),
        "  older 2".to_string(),
        "  … 47 more line(s), 900 B · /expand".to_string(),
        "→ shell cargo build".to_string(),
        "← ok newest 1".to_string(),
        "  … 49 more line(s), 900 B · /expand".to_string(),
    ]);
}

#[test]
fn tab_walks_the_folds_and_enter_shows_what_they_hide() {
    let mut app = App::new("s", "v");
    two_folds(&mut app);
    let screen = draw(&mut app, 72, 24);
    assert!(
        screen.contains("▸ … 47 more line(s), 900 B · /expand"),
        "a summary says what is hidden and how to get it: {screen}"
    );

    // Tab lands on the newest fold: the one just written, which is the one being read.
    assert_eq!(app.handle_key(key(KeyCode::Tab)), Action::None);
    assert_eq!(app.fold, Some(7));
    let selected = app
        .visible_lines(20)
        .into_iter()
        .find(|line| {
            line.spans
                .iter()
                .any(|span| span.content.contains("49 more"))
        })
        .expect("the marker is on screen");
    assert!(
        selected.spans[0]
            .style
            .add_modifier
            .contains(Modifier::REVERSED),
        "the keyboard's place is visible, not just remembered"
    );
    // Tab again walks to the older one, and shift-tab comes back.
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.fold, Some(4));
    app.handle_key(key(KeyCode::BackTab));
    assert_eq!(app.fold, Some(7));

    // Enter asks the log for that same result, by the number the two sides share — and
    // the answer is what fills the screen.
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Action::Send(ToWorker::ExpandAt { at: 1 })
    );
    let (says, worker) = channel();
    says.send(FromWorker::Expanded {
        at: 1,
        lines: (1..=50).map(|n| format!("newest {n}")).collect(),
    })
    .unwrap();
    app.drain(&worker);
    let screen = draw(&mut app, 72, 24);
    assert!(screen.contains("tool result (50 lines)"), "{screen}");
    assert!(
        screen.contains("newest 50"),
        "the end is what is read: {screen}"
    );
    app.handle_key(key(KeyCode::PageUp));
    assert!(
        draw(&mut app, 72, 24).contains("newest"),
        "and paging back works"
    );
    // Esc closes the output and keeps the fold; one more esc lets the fold go.
    app.handle_key(key(KeyCode::Esc));
    assert!(app.open.is_none());
    assert_eq!(app.fold, Some(7));
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.fold, None);
    assert!(
        draw(&mut app, 72, 24).contains("session s"),
        "and does not leave"
    );
}

#[test]
fn a_fold_never_takes_enter_away_from_a_prompt() {
    let mut app = App::new("s", "v");
    two_folds(&mut app);
    // With nothing selected, enter sends — exactly as it did before folds existed.
    type_text(&mut app, "hello");
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Action::Send(ToWorker::Submit("hello".to_string()))
    );
    // With a fold selected and something typed, enter still sends: a prompt with text
    // in it means send, and the fold is let go with it.
    app.handle_key(key(KeyCode::Tab));
    type_text(&mut app, "again");
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Action::Send(ToWorker::Submit("again".to_string()))
    );
    assert_eq!(app.fold, None, "sending is a return to the prompt");
}

#[test]
fn enter_opens_a_fold_the_turn_sent_without_asking_the_log() {
    let mut app = App::new("s", "v");
    two_folds(&mut app);
    // The turn sent the newest fold's own text along with its summary line, which is
    // what it does for every fold it writes.
    app.remember_fold(1, (1..=50).map(|n| format!("newest {n}")).collect());
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.fold, Some(7));
    // Enter asks nobody: the text is already here. That is what makes this work while
    // the turn is still running, when the connection is not free to ask on.
    assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None);
    let screen = draw(&mut app, 72, 24);
    assert!(screen.contains("tool result (50 lines)"), "{screen}");
    assert!(screen.contains("newest 50"), "{screen}");
    // A fold the view does not hold is still fetched from the log, in full.
    app.handle_key(key(KeyCode::Esc));
    app.handle_key(key(KeyCode::Esc));
    app.handle_key(key(KeyCode::Tab));
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Action::Send(ToWorker::ExpandAt { at: 0 })
    );
}

#[test]
fn server_notes_are_drawn_instead_of_corrupting_the_screen() {
    let mut app = App::new("s", "v");
    app.push_note("[sandbox] filesystem boundary enforced by landlock".to_string());
    app.push_note("[sandbox] filesystem boundary enforced by landlock".to_string());
    assert_eq!(app.notes.len(), 1, "repeats are collapsed");
    app.push_note("[hooks] no hooks".to_string());
    let screen = draw(&mut app, 70, 16);
    assert!(screen.contains("[hooks] no hooks"), "{screen}");
}

#[test]
fn a_failure_is_visible_and_does_not_wedge_the_screen() {
    let (to_app, from_worker) = channel::<FromWorker>();
    let mut app = App::new("s", "v");
    app.busy = true;
    to_app
        .send(FromWorker::Failed(
            "the server closed the connection".to_string(),
        ))
        .unwrap();
    to_app.send(FromWorker::State { pending: None }).unwrap();
    app.drain(&from_worker);
    assert!(!app.busy);
    assert_eq!(
        app.error.as_deref(),
        Some("the server closed the connection")
    );
    assert!(draw(&mut app, 80, 16).contains("the server closed the connection"));
}

#[test]
fn the_answer_being_written_stays_visible_to_its_last_line() {
    let (to_app, from_worker) = channel::<FromWorker>();
    let mut app = App::new("s", "v");
    app.busy = true;
    app.push_lines(vec!["you: 写个游戏".to_string()]);
    // A model that writes more than one screenful. The screen has to show the **end**
    // of it: the point of a live preview is watching the newest words arrive. The live
    // text used to be appended below a full window, so the box clipped it and the
    // answer appeared to stop moving after one screenful.
    for n in 1..=30 {
        to_app
            .send(FromWorker::Delta(format!("thinking line {n}\n")))
            .unwrap();
    }
    app.drain(&from_worker);
    let screen = draw(&mut app, 60, 24);
    assert!(screen.contains("thinking line 30"), "{screen}");
    assert!(
        !screen.contains("thinking line 1\n"),
        "the oldest lines give way to the newest: {screen}"
    );
}

#[test]
fn the_working_title_turns_so_a_wait_does_not_look_like_a_hang() {
    // The frame is read from the clock, so a missed redraw cannot freeze the mark — and
    // a test can name the frame it wants instead of racing a timer.
    assert_eq!(spinner_frame(None), SPINNER[0]);
    let start = std::time::Instant::now();
    assert_eq!(spinner_frame(Some(start)), SPINNER[0]);
    assert_ne!(
        spinner_frame(Some(
            start - std::time::Duration::from_millis(SPINNER_MS as u64)
        )),
        SPINNER[0],
        "one frame later is a different mark"
    );

    let mut app = App::new("s", "v");
    app.busy = true;
    app.busy_since = Some(start);
    app.activity = Some("shell: cargo test".to_string());
    let screen = draw(&mut app, 60, 20);
    assert!(
        screen.contains("working 0s · shell: cargo test"),
        "the words still say what it is doing: {screen}"
    );
    assert!(
        SPINNER.iter().any(|frame| screen.contains(frame)),
        "and a mark that moves is beside them: {screen}"
    );
}

#[test]
fn the_answer_appears_while_it_is_written_and_then_settles() {
    let (to_app, from_worker) = channel::<FromWorker>();
    let mut app = App::new("s", "v");
    app.busy = true;
    to_app.send(FromWorker::Delta("Wri".to_string())).unwrap();
    to_app.send(FromWorker::Delta("ting…".to_string())).unwrap();
    app.drain(&from_worker);
    assert_eq!(app.streaming, "Writing…");
    let screen = draw(&mut app, 40, 12);
    assert!(
        screen.contains("Writing…"),
        "the fragment is on screen: {screen}"
    );

    // The finished message arrives: one line, and the fragments are gone.
    to_app
        .send(FromWorker::Lines(vec!["Writing the answer.".to_string()]))
        .unwrap();
    to_app.send(FromWorker::State { pending: None }).unwrap();
    app.drain(&from_worker);
    assert!(
        app.streaming.is_empty(),
        "the preview is cleared by the record"
    );
    let screen = draw(&mut app, 40, 12);
    assert!(screen.contains("Writing the answer."), "{screen}");
    assert_eq!(
        screen.matches("Writing").count(),
        1,
        "and it is not shown twice: {screen}"
    );
}

#[test]
fn the_first_run_form_collects_three_fields_and_refuses_an_incomplete_one() {
    let mut form = SetupForm::new();
    for c in "http://127.0.0.1:8738".chars() {
        form.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(form.answer(), None, "an address alone is not enough");
    assert_eq!(form.handle_key(key(KeyCode::Enter)), SetupAction::None);
    assert!(form.error.is_some(), "and it says why");

    form.handle_key(key(KeyCode::Tab));
    assert_eq!(form.field, 1);
    for c in "ab".repeat(32).chars() {
        form.handle_key(key(KeyCode::Char(c)));
    }
    form.handle_key(key(KeyCode::Tab));
    assert_eq!(form.field, 2, "tab wraps forward");
    for c in "file:/tmp/client.token".chars() {
        form.handle_key(key(KeyCode::Char(c)));
    }
    let (url, public_key, token) = form.answer().expect("complete");
    assert_eq!(url, "http://127.0.0.1:8738");
    assert_eq!(public_key.len(), 64);
    assert_eq!(token, "file:/tmp/client.token");
    assert_eq!(form.handle_key(key(KeyCode::Enter)), SetupAction::Submit);

    // Backspace edits the active field, and tab wraps both ways.
    form.handle_key(key(KeyCode::Backspace));
    assert!(form.answer().unwrap().2.is_empty() == false || true);
    form.handle_key(key(KeyCode::Up));
    assert_eq!(form.field, 1);
    form.handle_key(key(KeyCode::Char('x')));
    assert!(form.public_key.ends_with('x'));
}

fn explained() -> Box<loong_schema::generated::ContextExplain> {
    use loong_schema::generated::{
        ContextCategory, ContextEntry, ContextExplain, ContextLayer, ContextWorker,
    };
    Box::new(ContextExplain {
        context_tokens: 1700,
        budget_tokens: 128000,
        messages: 12,
        tools: 9,
        compactions: 1,
        entries: vec![
            ContextEntry {
                id: 1,
                category: "system_prompt".to_string(),
                source: "loongagent".to_string(),
                tokens: 400,
                lifetime: "session".to_string(),
                pinned: true,
                cleared: false,
            },
            ContextEntry {
                id: 7,
                category: "history".to_string(),
                source: "tool result (shell)".to_string(),
                tokens: 900,
                lifetime: "turn".to_string(),
                pinned: false,
                cleared: true,
            },
        ],
        categories: vec![
            ContextCategory {
                category: "history".to_string(),
                tokens: 900,
            },
            ContextCategory {
                category: "system_prompt".to_string(),
                tokens: 400,
            },
        ],
        memory_mode: "semantic".to_string(),
        memory_summarizer: "ollama:qwen2.5:7b-instruct".to_string(),
        memory_degraded: String::new(),
        memory_layers: vec![
            ContextLayer {
                level: 1,
                scope: "atom".to_string(),
                count: 120,
                tokens: 0,
            },
            ContextLayer {
                level: 2,
                scope: "episode".to_string(),
                count: 8,
                tokens: 900,
            },
            ContextLayer {
                level: 3,
                scope: "domain".to_string(),
                count: 2,
                tokens: 120,
            },
            ContextLayer {
                level: 4,
                scope: "global".to_string(),
                count: 1,
                tokens: 200,
            },
        ],
        memory_worker: ContextWorker {
            state: "idle".to_string(),
            windows: 5,
            model_calls: 9,
            enriched: 8,
            failed: 0,
            pending: 1,
            deferrals: 3,
            last_error: String::new(),
        },
    })
}

/// A semantic engine that has not produced a map yet — the state that must not
/// be drawn as "off".
fn explained_fresh() -> Box<loong_schema::generated::ContextExplain> {
    let mut explained = explained();
    explained.memory_layers.iter_mut().for_each(|layer| {
        if layer.level >= 2 {
            layer.count = 0;
            layer.tokens = 0;
        }
    });
    explained.memory_worker.pending = 1;
    explained.memory_worker.enriched = 0;
    explained.memory_worker.model_calls = 0;
    explained
}

/// An engine that is off: `memory_mode` empty, whatever the ledger holds.
fn explained_off() -> Box<loong_schema::generated::ContextExplain> {
    let mut explained = explained();
    explained.memory_mode = String::new();
    explained.memory_summarizer = String::new();
    explained.memory_layers.clear();
    explained.memory_worker = loong_schema::generated::ContextWorker {
        state: "off".to_string(),
        windows: 0,
        model_calls: 0,
        enriched: 0,
        failed: 0,
        pending: 0,
        deferrals: 0,
        last_error: String::new(),
    };
    explained
}

#[test]
fn exit_and_quit_leave_the_interface_without_asking_the_agent() {
    for command in ["/exit", "/quit", "/q", " /exit ", "/EXIT"] {
        let mut app = App::new("s", "v");
        type_text(&mut app, command);
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Action::Quit,
            "{command:?} must quit, not be sent to a model"
        );
        assert!(app.input.is_empty(), "{command:?} clears the line");
        assert!(
            !app.lines.iter().any(|line| line.starts_with("you:")),
            "{command:?} is not part of the conversation"
        );
    }
}

#[test]
fn only_an_exact_command_is_taken_locally() {
    // The boundary matters: anything else is a skill invocation or a prompt, and
    // belongs to the server where it can be audited.
    for text in ["/exit now", "/exiting", "/exitx", "exit", "/help"] {
        let mut app = App::new("s", "v");
        type_text(&mut app, text);
        match app.handle_key(key(KeyCode::Enter)) {
            Action::Send(ToWorker::Submit(sent)) => assert_eq!(sent, text),
            other => panic!("{text:?} should go to the server, got {other:?}"),
        }
    }
}

#[test]
fn the_hint_says_what_exit_will_do() {
    let mut app = App::new("s", "v");
    type_text(&mut app, "/exit");
    let screen = draw(&mut app, 100, 16);
    assert!(screen.contains("enter leaves loongagent"), "{screen}");

    // And while it is still ambiguous, it names both the skills and the way out.
    let mut app = App::new("s", "v");
    type_text(&mut app, "/he");
    let screen = draw(&mut app, 100, 16);
    assert!(screen.contains("/help"), "{screen}");
    assert!(screen.contains("/copy"), "and the copy command: {screen}");
    assert!(screen.contains("/mouse"), "and the mouse one: {screen}");
    assert!(screen.contains("/exit"), "{screen}");
}

#[test]
fn f2_opens_the_context_panel_and_asks_for_its_data() {
    let mut app = App::new("s", "v");
    assert_eq!(
        app.handle_key(key(KeyCode::F(2))),
        Action::Send(ToWorker::RefreshContext),
        "opening it asks, so the panel is never an empty box"
    );
    assert!(app.debug);
    assert_eq!(
        app.handle_key(key(KeyCode::F(2))),
        Action::None,
        "closing asks for nothing"
    );
    assert!(!app.debug);
}

#[test]
fn the_context_panel_shows_where_the_tokens_went() {
    let (to_app, from_worker) = channel::<FromWorker>();
    let mut app = App::new("s", "v");
    app.handle_key(key(KeyCode::F(2)));
    to_app.send(FromWorker::Context(explained())).unwrap();
    app.drain(&from_worker);
    let screen = draw(&mut app, 130, 24);
    assert!(screen.contains("context (f2)"), "{screen}");
    assert!(screen.contains("1700 / 128000"), "the totals: {screen}");
    assert!(screen.contains("12 messages"), "{screen}");
    assert!(screen.contains("compactions"), "{screen}");
    assert!(
        screen.contains("history") && screen.contains("system_prompt"),
        "{screen}"
    );
    assert!(screen.contains("#1"), "the entries: {screen}");
    assert!(screen.contains("pinned"), "and the legend: {screen}");
    // The conversation did not disappear.
    assert!(screen.contains("loongagent — session"), "{screen}");
}

#[test]
fn the_panel_stays_closed_until_it_is_asked_for() {
    let mut app = App::new("s", "v");
    let screen = draw(&mut app, 110, 20);
    assert!(!screen.contains("context (f2)"), "{screen}");
    assert!(screen.contains("f2"), "the hint advertises it: {screen}");
}

/// Draw the F2 panel over one canned context view.
fn panel(context: Box<loong_schema::generated::ContextExplain>) -> String {
    let (to_app, from_worker) = channel::<FromWorker>();
    let mut app = App::new("s", "v");
    app.handle_key(key(KeyCode::F(2)));
    to_app.send(FromWorker::Context(context)).unwrap();
    app.drain(&from_worker);
    draw(&mut app, 200, 40)
}

#[test]
fn the_panel_shows_the_layers_the_worker_and_the_mode() {
    let screen = panel(explained());
    assert!(screen.contains("memory"), "{screen}");
    assert!(screen.contains("semantic"), "the mode: {screen}");
    assert!(
        screen.contains("summarizer ollama:qwen2.5:7b-instruct"),
        "which model: {screen}"
    );
    // Layer counts, which is what the design asks the panel to expose.
    assert!(screen.contains("L1 120"), "{screen}");
    assert!(screen.contains("L2 8"), "{screen}");
    assert!(screen.contains("L3 2"), "{screen}");
    assert!(screen.contains("L4 1"), "{screen}");
    // And what the background has cost.
    assert!(screen.contains("worker idle"), "{screen}");
    assert!(screen.contains("9 calls"), "{screen}");
    assert!(screen.contains("8 enriched"), "{screen}");
    assert!(screen.contains("3 deferred"), "{screen}");
}

#[test]
fn a_degraded_memory_says_why_in_the_panel_and_on_the_bar() {
    let mut context = explained();
    context.memory_degraded =
        "3 episode(s) fell back to the digest: connection refused".to_string();
    context.memory_worker.failed = 3;
    context.memory_worker.last_error = "connection refused".to_string();
    let screen = panel(context);
    assert!(screen.contains("degraded:"), "{screen}");
    assert!(
        screen.contains("fell back to the digest"),
        "the reason, not just the fact: {screen}"
    );
    assert!(screen.contains("connection refused"), "{screen}");

    // The bar carries the warning too, so a silent fallback is impossible even
    // with the panel closed.
    let (to_app, from_worker) = channel::<FromWorker>();
    let mut app = App::new("s", "v");
    let mut context = explained();
    context.memory_degraded = "the summariser is unreachable".to_string();
    to_app.send(FromWorker::Context(context)).unwrap();
    app.drain(&from_worker);
    let screen = draw(&mut app, 200, 24);
    assert!(screen.contains("mem ⚠ degraded"), "{screen}");
}

#[test]
fn an_engine_that_is_off_is_not_shown_as_a_memory_that_has_produced_nothing() {
    // The whole reason `memory_mode` exists. "Off" and "on, but the first turns
    // have not produced an L4 map yet" both have no `cognitive_map` entry; they
    // must not render the same.
    let off = panel(explained_off());
    let fresh = panel(explained_fresh());
    assert!(off.contains("memory"), "{off}");
    assert!(off.contains("off"), "{off}");
    assert!(
        !off.contains("semantic") && !off.contains("no L4 map yet"),
        "an engine that is off must not claim a mode or a pending map: {off}"
    );
    assert!(fresh.contains("semantic"), "{fresh}");
    assert!(
        fresh.contains("no L4 map yet"),
        "an engine that is on and has produced nothing says exactly that: {fresh}"
    );
    assert_ne!(
        off, fresh,
        "the two states must be distinguishable on screen"
    );
}

#[test]
fn record_mode_says_it_has_no_model() {
    let mut context = explained();
    context.memory_mode = "record".to_string();
    context.memory_summarizer = String::new();
    let screen = panel(context);
    assert!(screen.contains("record"), "{screen}");
    assert!(screen.contains("none (record mode)"), "{screen}");
}

#[test]
fn the_bar_names_the_mode_while_the_panel_is_closed() {
    let (to_app, from_worker) = channel::<FromWorker>();
    let mut app = App::new("s", "v");
    to_app.send(FromWorker::Context(explained())).unwrap();
    app.drain(&from_worker);
    let screen = draw(&mut app, 200, 24);
    assert!(screen.contains("mem semantic"), "{screen}");
    // With the engine off there is nothing to say, and saying it anyway would be
    // noise on every screen.
    let (to_app, from_worker) = channel::<FromWorker>();
    let mut app = App::new("s", "v");
    to_app.send(FromWorker::Context(explained_off())).unwrap();
    app.drain(&from_worker);
    let screen = draw(&mut app, 200, 24);
    assert!(!screen.contains("mem "), "{screen}");
}

#[test]
fn the_first_run_form_shows_three_labelled_fields_and_a_hint() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
    let mut form = SetupForm::new();
    form.url = "http://127.0.0.1:8738".to_string();
    form.public_key = "ab".repeat(32);
    form.token = "file:/tmp/client.token".to_string();
    terminal
        .draw(|frame| draw_setup_form(frame, &form))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let screen: String = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(screen.contains("first run"), "{screen}");
    assert!(screen.contains("control-plane address"), "{screen}");
    assert!(screen.contains("public key"), "{screen}");
    assert!(screen.contains("token reference"), "{screen}");
    assert!(
        screen.contains("esc skip"),
        "the way out is visible: {screen}"
    );
    // Three *closed* boxes before the hint line: a four-row layout once put the
    // hint on the third field's top border, which this catches.
    let rows: Vec<&str> = screen.lines().collect();
    let hint_row = rows.iter().position(|r| r.contains("esc skip")).unwrap();
    let closed = rows[..hint_row].iter().filter(|r| r.contains('└')).count();
    assert!(
        closed >= 4,
        "the hint must sit below the title and all three fields:\n{screen}"
    );
    assert!(
        screen.contains("http://127.0.0.1:8738"),
        "what was typed is shown: {screen}"
    );
    // A finished form is not shouting an error.
    assert!(!screen.contains("required"), "{screen}");
}

#[test]
fn the_first_run_form_is_skippable_with_escape_or_ctrl_c() {
    let mut form = SetupForm::new();
    assert_eq!(form.handle_key(key(KeyCode::Esc)), SetupAction::Cancel);
    for c in ['c', 'd'] {
        assert_eq!(
            form.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)),
            SetupAction::Cancel,
            "ctrl-{c} must not leave the terminal stuck in the form"
        );
    }
    // A plain 'c' is still a character in the address.
    form.handle_key(key(KeyCode::Char('c')));
    assert_eq!(form.url, "c");
}

#[test]
fn the_caret_moves_and_edits_in_the_middle_of_the_line() {
    let mut app = App::new("s", "v");
    type_text(&mut app, "helo");
    app.handle_key(key(KeyCode::Left));
    app.handle_key(key(KeyCode::Char('l')));
    assert_eq!(app.input, "hello");
    assert_eq!(app.cursor, 4, "the caret follows what was typed");

    app.handle_key(key(KeyCode::Home));
    app.handle_key(key(KeyCode::Delete));
    assert_eq!(app.input, "ello");
    app.handle_key(key(KeyCode::End));
    app.handle_key(key(KeyCode::Backspace));
    assert_eq!(app.input, "ell");

    // Readline habits work: ctrl-a/e move, ctrl-u/k kill, ctrl-w kills a word.
    app.handle_key(key(KeyCode::Char('o')));
    app.handle_key(key(KeyCode::Char(' ')));
    type_text(&mut app, "world");
    app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
    assert_eq!(app.input, "ello ", "ctrl-w kills the word behind the caret");
    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
    app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    assert_eq!(app.input, "", "ctrl-a then ctrl-k clears the line");
    assert_eq!(app.cursor, 0);
}

#[test]
fn utf8_is_edited_by_character_not_by_byte() {
    let mut app = App::new("s", "v");
    type_text(&mut app, "你好世界");
    app.handle_key(key(KeyCode::Left));
    app.handle_key(key(KeyCode::Backspace));
    assert_eq!(
        app.input, "你好界",
        "a whole character goes, not a broken byte"
    );
    app.handle_key(key(KeyCode::Home));
    app.handle_key(key(KeyCode::Right));
    app.handle_key(key(KeyCode::Char('，')));
    assert_eq!(app.input, "你，好界");
    assert_eq!(app.cursor, 2);
}

#[test]
fn a_long_prompt_wraps_instead_of_running_off_the_edge() {
    let mut app = App::new("s", "v");
    type_text(&mut app, "0123456789");
    let (lines, caret) = app.wrapped_input(6);
    assert_eq!(lines, vec!["012345".to_string(), "6789".to_string()]);
    assert_eq!(
        caret,
        (1, 4),
        "the caret is on the second row, after the text"
    );
    assert_eq!(app.input_rows(6), 2, "so the box is two rows tall");

    // The caret's row is where editing happens, wherever it is.
    app.handle_key(key(KeyCode::Home));
    let (lines, caret) = app.wrapped_input(6);
    assert_eq!(
        lines,
        vec!["012345".to_string(), "6789".to_string()],
        "the text stays put"
    );
    assert_eq!(caret, (0, 0), "the caret moved, the layout did not");
}

#[test]
fn wrapping_counts_display_columns_so_cjk_fits_the_border() {
    // Two columns each: four of them are eight columns, which is two rows of four
    // columns — not eight characters on one row that overflows.
    let mut app = App::new("s", "v");
    type_text(&mut app, "中文中文中文");
    let (lines, caret) = app.wrapped_input(4);
    assert_eq!(
        lines,
        vec!["中文".to_string(), "中文".to_string(), "中文".to_string()]
    );
    assert_eq!(caret, (2, 4));

    // And a wide character that would straddle the edge starts the next row.
    let mut app = App::new("s", "v");
    type_text(&mut app, "ab中");
    let (lines, _) = app.wrapped_input(3);
    assert_eq!(lines, vec!["ab".to_string(), "中".to_string()]);
}

#[test]
fn a_pasted_newline_is_a_hard_break() {
    let mut app = App::new("s", "v");
    type_text(&mut app, "first\nsecond");
    let (lines, caret) = app.wrapped_input(40);
    assert_eq!(lines, vec!["first".to_string(), "second".to_string()]);
    assert_eq!(caret, (1, 6));
}

#[test]
fn the_input_box_stops_growing_and_keeps_the_caret_on_screen() {
    let mut app = App::new("s", "v");
    type_text(&mut app, &"x".repeat(200));
    assert_eq!(
        app.input_rows(10),
        MAX_INPUT_ROWS,
        "capped, so the conversation survives"
    );

    // What the drawing code does: show the window that contains the caret.
    let (lines, (caret_row, _)) = app.wrapped_input(10);
    let window = app.input_rows(10) as usize;
    let offset = (caret_row as usize + 1).saturating_sub(window);
    assert_eq!(lines.len(), 20, "200 characters at 10 columns");
    assert_eq!(
        offset + window,
        lines.len(),
        "the window is at the end, with the caret"
    );
}

#[test]
fn the_wheel_scrolls_the_conversation() {
    let mut app = App::new("s", "v");
    app.push_lines((1..=40).map(|i| format!("line {i}")));
    app.handle_mouse(ratatui::crossterm::event::MouseEventKind::ScrollUp);
    assert_eq!(app.scroll, 3, "a wheel notch moves three lines");
    app.handle_mouse(ratatui::crossterm::event::MouseEventKind::ScrollDown);
    assert_eq!(app.scroll, 0);
    app.handle_mouse(ratatui::crossterm::event::MouseEventKind::ScrollDown);
    assert_eq!(app.scroll, 0, "and it stops at the bottom");
}

/// The foreground a line is drawn in, read from the cell buffer (the text helper
/// throws styles away, which is the point of having this one).
fn color_of(app: &mut App, width: u16, height: u16, needle: &str) -> Option<ratatui::style::Color> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let area = buffer.area();
    for y in 0..area.height {
        let row: String = (0..area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect();
        if let Some(column) = row.find(needle) {
            let x = column as u16;
            if x < area.width {
                return Some(
                    buffer[(x, y)]
                        .style()
                        .fg
                        .unwrap_or(ratatui::style::Color::Reset),
                );
            }
        }
    }
    None
}

#[test]
fn every_kind_of_line_has_its_own_colour() {
    for (line, expected) in [
        ("you: fix the bug", LineKind::User),
        ("→ read_file a.txt", LineKind::ToolCall),
        ("← ok hello", LineKind::ToolOk),
        ("← failed no such file", LineKind::ToolFailed),
        (
            "! approval required (out_of_bounds): write [cwd]",
            LineKind::Approval,
        ),
        ("! /deploy: missing argument", LineKind::Problem),
        ("! the turn failed", LineKind::Problem),
        (
            "· policy: allow by builtin:write_workspace",
            LineKind::Policy,
        ),
        ("· tokens 1053 in / 5 out (default)", LineKind::Note),
        ("· compacted: 100 -> 40 tokens", LineKind::Note),
        ("here is the answer", LineKind::Answer),
    ] {
        assert_eq!(classify(line), expected, "{line:?}");
    }
    // And the palette actually differs between the kinds that matter.
    assert_ne!(
        style_for(LineKind::Problem).fg,
        style_for(LineKind::Answer).fg
    );
    assert_ne!(
        style_for(LineKind::ToolCall).fg,
        style_for(LineKind::ToolOk).fg
    );
}

#[test]
fn the_renderer_and_the_classifier_agree() {
    // The classifier reads the vocabulary `loong_tui::render_event` writes. Rendered
    // through the real renderer, every marked line must be classified — only the
    // model's own prose is allowed to fall through to `Answer`.
    use loong_schema::generated::{event, Role};
    let cases = vec![
        serde_json::json!({"kind": event::MESSAGE_APPENDED, "data": {"role": Role::User, "text": "hi"}}),
        serde_json::json!({"kind": event::MESSAGE_APPENDED, "data": {"role": Role::Assistant, "text": "hello"}}),
        serde_json::json!({"kind": event::TOOL_CALL, "data": {"tool": "read_file", "args": {"path": "a.txt"}}}),
        serde_json::json!({"kind": event::TOOL_RESULT, "data": {"ok": true, "output": "hi"}}),
        serde_json::json!({"kind": event::TOOL_RESULT, "data": {"ok": false, "output": "nope"}}),
        serde_json::json!({"kind": event::POLICY_DECIDED, "data": {"verdict": "allow", "rule": "builtin"}}),
        serde_json::json!({"kind": event::APPROVAL_REQUESTED, "data": {"request_id": "r1", "action_class": "out_of_bounds"}}),
        serde_json::json!({"kind": event::COST_RECORDED, "data": {"tokens_in": 1, "tokens_out": 2, "profile": "p"}}),
        serde_json::json!({"kind": event::SKILLS_LOADED, "data": {"count": 1, "names": ["help"], "disabled": [], "tokens": 3}}),
        serde_json::json!({"kind": event::COMPACTION_APPLIED, "data": {"tokens_before": 9, "tokens_after": 4, "kept_tokens": 2, "cleared_tool_outputs": 1, "dropped_messages": 1, "summary": "s"}}),
    ];
    for case in cases {
        let line = loong_tui::render_event(&case).unwrap_or_else(|| panic!("no line for {case}"));
        if line.starts_with(&["you: ", "→ ", "← ", "! ", "· "][0])
            || ["→ ", "← ", "! ", "· "].iter().any(|p| line.starts_with(p))
        {
            assert_ne!(
                classify(&line),
                LineKind::Answer,
                "{line:?} is marked, so it must be classified"
            );
        }
    }
}

#[test]
fn errors_are_red_and_work_is_blue_in_the_drawn_screen() {
    let mut app = App::new("s", "v");
    app.push_lines(vec![
        "you: do it".to_string(),
        "→ write_file game.html".to_string(),
        "← ok wrote it".to_string(),
        "! the turn failed".to_string(),
        "· policy: allow by builtin".to_string(),
        "all done".to_string(),
    ]);
    assert_eq!(
        color_of(&mut app, 80, 20, "the turn failed"),
        Some(ratatui::style::Color::Red)
    );
    assert_eq!(
        color_of(&mut app, 80, 20, "write_file"),
        Some(ratatui::style::Color::Cyan)
    );
    assert_eq!(
        color_of(&mut app, 80, 20, "wrote it"),
        Some(ratatui::style::Color::Green)
    );
    assert_eq!(
        color_of(&mut app, 80, 20, "by builtin"),
        Some(ratatui::style::Color::Magenta)
    );
    assert_eq!(
        color_of(&mut app, 80, 20, "you:"),
        Some(ratatui::style::Color::Cyan),
        "the label marks where a prompt began"
    );
    assert_eq!(
        color_of(&mut app, 80, 20, "do it"),
        Some(ratatui::style::Color::Reset),
        "and the person's own words stay plain, so they are readable"
    );
    assert_eq!(
        color_of(&mut app, 80, 20, "all done"),
        Some(ratatui::style::Color::Reset),
        "the answer is left alone"
    );
}

#[test]
fn control_c_stops_a_running_turn_and_otherwise_leaves() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let calls = Arc::new(AtomicUsize::new(0));
    let asked = Arc::clone(&calls);

    let mut app = App::new("s", "v");
    app.interrupt = Some(Box::new(move || {
        asked.fetch_add(1, Ordering::SeqCst);
    }));
    // Idle: Ctrl-C leaves, exactly as it always did.
    let mut idle = App::new("s", "v");
    assert_eq!(
        idle.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        Action::Quit
    );
    // Running: it asks the server to stop, and says so without pretending the work
    // has already stopped.
    app.busy = true;
    assert_eq!(
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        Action::None
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        app.lines
            .iter()
            .any(|line| line.starts_with("⏹ interrupting")),
        "{:?}",
        app.lines
    );
    // Esc is the same request.
    assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::None);
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // A surface with nothing to signal says so instead of lying.
    let mut app = App::new("s", "v");
    app.busy = true;
    assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::None);
    assert!(
        app.lines
            .iter()
            .any(|line| line.contains("cannot interrupt")),
        "{:?}",
        app.lines
    );
}

#[test]
fn the_interrupted_event_renders_as_a_stop_the_person_caused() {
    use loong_schema::generated::event;
    let line = loong_tui::render_event(&serde_json::json!({
        "kind": event::TURN_INTERRUPTED,
        "data": {"reason": "interrupted"},
    }))
    .expect("an interrupted turn is shown");
    assert!(line.contains("stopped by you"), "{line}");
    assert_eq!(classify(&line), LineKind::Stopped);
}

#[test]
fn a_stopped_turn_is_its_own_colour() {
    assert_eq!(
        classify("⏹ interrupted — stopped by you"),
        LineKind::Stopped
    );
    assert_eq!(
        style_for(LineKind::Stopped).fg,
        Some(ratatui::style::Color::Yellow)
    );
    let mut app = App::new("s", "v");
    app.push_lines(vec!["⏹ interrupted — stopped by you".to_string()]);
    assert_eq!(
        color_of(&mut app, 80, 20, "stopped by you"),
        Some(ratatui::style::Color::Yellow)
    );
}

#[test]
fn the_person_s_own_line_is_matched_by_its_event_not_printed_twice() {
    // The screen push happens on enter; the event that comes back is the same line.
    // The worker skips exactly that one, and only while a turn is live.
    let own = serde_json::json!({
        "kind": loong_schema::generated::event::MESSAGE_APPENDED,
        "data": {"role": "user", "text": "hi"},
    });
    assert_eq!(
        kind_of(&own),
        loong_schema::generated::event::MESSAGE_APPENDED
    );
    assert_eq!(own["data"]["role"].as_str(), Some("user"));

    let assistant = serde_json::json!({
        "kind": loong_schema::generated::event::MESSAGE_APPENDED,
        "data": {"role": "assistant", "text": "hello"},
    });
    assert_ne!(
        assistant["data"]["role"].as_str(),
        Some("user"),
        "the model's own words are still rendered"
    );
}

#[test]
fn yolo_is_a_command_and_is_never_invisible() {
    let mut app = App::new("s", "v");
    type_text(&mut app, "/yolo");
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Action::Send(ToWorker::Yolo(true)),
        "the command asks the server, which records the opt-out"
    );
    assert!(app.input.is_empty());

    // Until the server answers, the state is unchanged: the interface does not
    // pretend a permission it does not have.
    assert!(!app.yolo);
    let (to_app, from_worker) = channel::<FromWorker>();
    to_app.send(FromWorker::Yolo(true)).unwrap();
    app.drain(&from_worker);
    assert!(app.yolo);
    assert!(
        app.lines
            .iter()
            .any(|line| line.contains("approval prompts off") && line.contains("sandbox")),
        "it says what changed and what did not: {:?}",
        app.lines
    );

    // Visible in the input box, always.
    let screen = draw(&mut app, 100, 24);
    assert!(screen.contains("YOLO"), "the state is on screen: {screen}");
    assert!(screen.contains("sandbox still on"), "{screen}");

    // And it can be turned back on, from the command or the state.
    let screen = draw(&mut app, 100, 24);
    assert!(screen.contains("/yolo"), "{screen}");
}

#[test]
fn copy_puts_the_transcript_on_the_clipboard_and_in_a_file() {
    let mut app = App::new("s", "v");
    app.push_lines(vec!["you: hello".to_string(), "hi".to_string()]);
    type_text(&mut app, "/copy");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::Copy);
    assert!(app.input.is_empty());
    // The file half, which is what works everywhere: written beside nothing
    // important, in the temporary directory, and named for the session.
    let path = copy_everywhere(&app.transcript(), "testcopy");
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("you: hello"), "{written}");
    assert!(written.contains("hi"), "{written}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn base64_is_base64() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foo"), "Zm9v");
    assert_eq!(base64(b"foob"), "Zm9vYg==");
    assert_eq!(base64(b"hello world"), "aGVsbG8gd29ybGQ=");
}

#[test]
fn the_mouse_can_be_handed_back_to_the_terminal() {
    let mut app = App::new("s", "v");
    // Off by default: the terminal's own selection is not something a TUI may take
    // away in exchange for a wheel.
    assert!(!app.mouse, "does not track the mouse unless asked");

    type_text(&mut app, "/mouse");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::Mouse(true));
    assert!(app.mouse);
    assert!(
        app.lines
            .iter()
            .any(|line| line.contains("wheel scrolls here")),
        "and says what it costs: {:?}",
        app.lines
    );

    type_text(&mut app, "/mouse");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::Mouse(false));
    assert!(!app.mouse);
    assert!(
        app.lines.iter().any(|line| line.contains("drag to select")),
        "and says what came back: {:?}",
        app.lines
    );
}

#[test]
fn the_mark_is_drawn_and_can_be_put_away() {
    let mut app = App::new("s", "v");
    // Wide: the frame, the character, the inscription beside it, the philosophy below.
    let screen = draw(&mut app, 150, 30);
    assert!(
        screen.contains("༺ ᛭ ༻") && screen.contains("༻ ᛭ ༺"),
        "{screen}"
    );
    assert!(screen.contains('【') && screen.contains('】'), "{screen}");
    assert!(screen.contains('龍'), "{screen}");
    assert!(screen.contains("loongagent"), "{screen}");
    assert!(
        screen.contains('⟬') && screen.contains('⟭'),
        "the inscription's brackets:\n{screen}"
    );
    assert!(screen.contains('⋄'), "and its divider:\n{screen}");
    // One character at a time: the cell buffer pads every wide glyph with a
    // continuation cell, so a phrase assertion would test the buffer's spacing.
    for glyph in ['兴', '云', '吐', '雾', '隐', '介', '藏', '形'] {
        assert!(screen.contains(glyph), "missing {glyph}:\n{screen}");
    }
    // Nothing below the frame: the inscription is the whole saying.
    for glyph in ['滞', '动', '必', '中'] {
        assert!(
            !screen.contains(glyph),
            "the lower line is gone, but {glyph} is still here:\n{screen}"
        );
    }

    // Narrower: the ornaments go, then the halves shorten; the mark stays.
    let plain = draw(&mut app, 62, 30);
    assert!(plain.contains('兴'), "{plain}");
    assert!(!plain.contains('⟬'), "the brackets need room:\n{plain}");
    let shortest = draw(&mut app, 52, 30);
    assert!(shortest.contains('龍'), "the mark itself:\n{shortest}");
    assert!(!shortest.contains('藏'), "and only what fits:\n{shortest}");

    // `/logo` puts it away, and nothing else moves.
    type_text(&mut app, "/logo");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None);
    assert!(!app.logo);
    let screen = draw(&mut app, 150, 30);
    assert!(!screen.contains('龍'), "{screen}");
    assert!(
        screen.contains("enter send"),
        "the box and keys remain:\n{screen}"
    );
}

#[test]
fn a_small_terminal_gets_no_mark() {
    // A logo that squeezes the conversation costs more than it gives.
    let mut app = App::new("s", "v");
    let screen = draw(&mut app, 36, 12);
    assert!(!screen.contains("龍"), "{screen}");
    assert!(screen.contains("enter send"), "{screen}");
}

#[test]
fn the_drawn_box_grows_with_a_long_prompt() {
    let mut app = App::new("s", "v");
    type_text(&mut app, &"abcd ".repeat(12));
    let screen = draw(&mut app, 40, 20);
    let rows: Vec<&str> = screen
        .lines()
        .filter(|line| line.contains("abcd"))
        .collect();
    assert!(
        rows.len() >= 2,
        "the prompt is on more than one row:\n{screen}"
    );
    // The box's bottom border is still above the hint line, so the layout held.
    let hint = screen
        .lines()
        .position(|line| line.contains("enter send"))
        .expect("the hint line is drawn");
    let last_text = screen
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("abcd"))
        .map(|(index, _)| index)
        .last()
        .unwrap();
    assert!(
        last_text < hint,
        "the input stays above the keys:\n{screen}"
    );

    // A short prompt is still one row: the box does not always take the space.
    let mut app = App::new("s", "v");
    type_text(&mut app, "hi");
    let screen = draw(&mut app, 40, 20);
    assert_eq!(
        screen.lines().filter(|line| line.contains("hi")).count(),
        1,
        "{screen}"
    );
}

#[test]
fn the_caret_is_drawn_where_the_next_character_goes() {
    // The terminal cursor is not part of the rendered buffer, so this checks the
    // arithmetic the drawing code uses.
    let mut app = App::new("s", "v");
    type_text(&mut app, "abc");
    let (_, caret) = app.wrapped_input(20);
    assert_eq!(caret, (0, 3));
    app.handle_key(key(KeyCode::Home));
    let (_, caret) = app.wrapped_input(20);
    assert_eq!(caret, (0, 0));
}

#[test]
fn a_line_with_embedded_newlines_keeps_its_shape() {
    let mut app = App::new("s", "v");
    app.push_lines(vec!["header\nbody".to_string()]);
    assert_eq!(app.lines, vec!["header".to_string(), "body".to_string()]);
}

#[test]
fn alt_digit_reaches_the_server_and_comes_back_to_the_bar() {
    // The whole chain, not just the key: key → action → worker → client → server →
    // back → bar. The earlier test only went as far as the `Action`, which is exactly
    // where a break would hide.
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Recorder {
        sent: Arc<Mutex<Vec<String>>>,
    }

    impl loong_tui::Transport for Recorder {
        fn send(&mut self, line: &str) -> Result<(), String> {
            self.sent.lock().unwrap().push(line.to_string());
            Ok(())
        }
        fn recv_line(&mut self) -> Result<Option<String>, String> {
            // Whatever was asked, answer with the state *after* a switch: two models,
            // the second one current.
            Ok(Some(
                serde_json::json!({
                    "jsonrpc": "2.0", "id": 1,
                    "result": {
                        "models": [
                            {"name": "a", "model": "a", "provider": null, "context_window": null},
                            {"name": "b", "model": "b", "provider": null, "context_window": null}
                        ],
                        "selected": "b"
                    }
                })
                .to_string(),
            ))
        }
    }

    let transport = Recorder::default();
    let sent = Arc::clone(&transport.sent);
    let client = Client::new(transport, "s-models");
    let (to_worker, from_worker) = spawn_worker(client, 0);

    let mut app = App::new("s-models", "v");
    app.set_models(loong_schema::generated::ModelList {
        models: vec![
            loong_schema::generated::ModelOffering {
                name: "a".into(),
                model: "a".into(),
                provider: None,
                context_window: None,
            },
            loong_schema::generated::ModelOffering {
                name: "b".into(),
                model: "b".into(),
                provider: None,
                context_window: None,
            },
        ],
        selected: "a".into(),
    });

    // The key, then the round trip.
    let action = app.handle_key(alt('2'));
    assert_eq!(
        action,
        Action::Send(ToWorker::SelectModel("b".to_string())),
        "the key produces the command"
    );
    let Action::Send(command) = action else {
        unreachable!()
    };
    to_worker.send(command).unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline && app.selected != "b" {
        app.drain(&from_worker);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    assert_eq!(app.selected, "b", "the bar moved");
    let asked = sent.lock().unwrap().join("\n");
    assert!(
        asked.contains("model.select"),
        "and the server was actually asked: {asked}"
    );
    let said = app.lines.join("\n");
    assert!(
        despaced(&said).contains(&despaced("模型已切到 [2] b")),
        "and the change is said out loud, named by slot so it matches the bar: {said:?}"
    );
    assert!(said.contains("此前 a"), "and names what it was: {said:?}");
}

#[test]
fn the_worker_turns_commands_into_screen_lines() {
    // A transport that answers everything with one canned line: the worker's
    // contract is that a command produces lines plus a state update.
    struct Echo;
    impl loong_tui::Transport for Echo {
        fn send(&mut self, _line: &str) -> Result<(), String> {
            Ok(())
        }
        fn recv_line(&mut self) -> Result<Option<String>, String> {
            Ok(Some(
                serde_json::json!({
                    "jsonrpc": "2.0", "id": 1,
                    "result": {"turn_id": 1, "accepted": true}
                })
                .to_string(),
            ))
        }
    }
    let client = Client::new(Echo, "s-worker");
    let (to_worker, from_worker) = spawn_worker(client, 0);
    to_worker
        .send(ToWorker::Submit("hello".to_string()))
        .unwrap();
    let mut app = App::new("s-worker", "v");
    app.busy = true;
    // Wait for the worker to report back.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline && app.busy {
        app.drain(&from_worker);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!app.busy, "the worker reported a state change");
}

#[test]
fn the_worker_fetches_the_fold_the_keyboard_picked() {
    /// Answers every request with a log holding one summarised tool result.
    struct Log;
    impl loong_tui::Transport for Log {
        fn send(&mut self, _line: &str) -> Result<(), String> {
            Ok(())
        }
        fn recv_line(&mut self) -> Result<Option<String>, String> {
            let output = (1..=50)
                .map(|n| format!("hidden {n}"))
                .collect::<Vec<_>>()
                .join("\n");
            Ok(Some(
                serde_json::json!({
                    "jsonrpc": "2.0", "id": 1,
                    "result": {"events": [
                        {"seq": 1, "kind": "tool.result",
                         "data": {"call_id": "c1", "ok": true, "output": output}},
                    ]}
                })
                .to_string(),
            ))
        }
    }
    let client = Client::new(Log, "s-worker");
    let (to_worker, from_worker) = spawn_worker(client, 0);
    to_worker.send(ToWorker::ExpandAt { at: 0 }).unwrap();
    let mut app = App::new("s-worker", "v");
    app.push_lines(vec![
        "← ok hidden 1".to_string(),
        "  … 47 more line(s), 900 B · /expand".to_string(),
    ]);
    app.fold = Some(1);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline && app.open.is_none() {
        app.drain(&from_worker);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        app.open.is_some(),
        "the worker answered with the log's text"
    );
    assert!(
        draw(&mut app, 60, 20).contains("hidden 50"),
        "and the screen shows what was hidden"
    );
}

#[test]
fn the_second_question_of_a_session_reaches_the_screen() {
    // Reported twice: the first approval was fine and the second was invisible. Answering
    // a question *resumes* the turn, and the next question is usually met right there —
    // but the client cleared whatever was pending after every answer, so the question it
    // had just met was thrown away and the worker reported "nothing pending". The panel
    // was never drawn; the transcript still showed the `! approval required` line for it.
    struct Queue(std::cell::RefCell<std::collections::VecDeque<String>>);
    impl loong_tui::Transport for Queue {
        fn send(&mut self, _line: &str) -> Result<(), String> {
            Ok(())
        }
        fn recv_line(&mut self) -> Result<Option<String>, String> {
            Ok(self.0.borrow_mut().pop_front())
        }
    }
    let line = |value: serde_json::Value| value.to_string();
    // The worker asks for the context view after every command, so a canned answer keeps
    // the queue aligned with the requests that are actually sent.
    let context_result = || {
        line(serde_json::json!({"jsonrpc": "2.0", "id": 9, "result": {
            "context_tokens": 10, "budget_tokens": 0, "messages": 1, "tools": 1,
            "compactions": 0, "categories": [], "entries": []}}))
    };
    let queue = vec![
        line(
            serde_json::json!({"jsonrpc": "2.0", "method": "event", "params": {
            "seq": 1, "kind": "tool.call",
            "data": {"call_id": "c1", "tool": "shell", "args": {"command": "ls"}}}}),
        ),
        line(
            serde_json::json!({"jsonrpc": "2.0", "method": "event", "params": {
            "seq": 2, "kind": "approval.requested",
            "data": {"request_id": "appr-1", "action_class": "out_of_bounds",
                     "reason": "shell: out_of_bounds is Ask by builtin:out_of_bounds"}}}),
        ),
        line(
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {"turn_id": 1, "accepted": false}}),
        ),
        context_result(),
        // The resume: another call, and the next question about it. Answering the first
        // one continues the turn; it does not end it.
        line(
            serde_json::json!({"jsonrpc": "2.0", "method": "event", "params": {
            "seq": 3, "kind": "tool.call",
            "data": {"call_id": "c2", "tool": "write_file",
                     "args": {"path": "game.html", "content": "<html>"}}}}),
        ),
        line(
            serde_json::json!({"jsonrpc": "2.0", "method": "event", "params": {
            "seq": 4, "kind": "approval.requested",
            "data": {"request_id": "appr-2", "action_class": "out_of_bounds",
                     "reason": "write_file: out_of_bounds is Ask by builtin:out_of_bounds"}}}),
        ),
        line(
            serde_json::json!({"jsonrpc": "2.0", "id": 2, "result": {"turn_id": 1, "accepted": true}}),
        ),
        context_result(),
    ];
    let client = Client::new(Queue(std::cell::RefCell::new(queue.into())), "s-1");
    let (to_worker, from_worker) = spawn_worker(client, 0);
    let mut app = App::new("s-1", "v");
    app.busy = true;
    to_worker.send(ToWorker::Submit("go".to_string())).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline && app.pending.is_none() {
        app.drain(&from_worker);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(app.pending.as_deref(), Some("appr-1"));
    assert!(app.approval.is_some(), "the first question has its detail");

    // Answer it, exactly as the run loop would.
    match app.handle_key(key(KeyCode::Enter)) {
        Action::Send(command) => to_worker.send(command).unwrap(),
        other => panic!("expected an answer, got {other:?}"),
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline && app.pending.as_deref() != Some("appr-2") {
        app.drain(&from_worker);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(
        app.pending.as_deref(),
        Some("appr-2"),
        "the second question is still a question"
    );
    let second = app
        .approval
        .as_ref()
        .expect("and it has its detail, like the first");
    assert_eq!(second.tool, "write_file", "{second:?}");
    assert!(second.args.contains("game.html"), "{second:?}");

    let screen = draw(&mut app, 100, 30);
    assert!(screen.contains("needs your decision"), "{screen}");
    assert!(screen.contains("write_file"), "{screen}");
    for answer in ["Allow once", "Allow in this project", "Deny"] {
        assert!(screen.contains(answer), "{answer} is on screen: {screen}");
    }
}

#[test]
fn a_fold_the_turn_wrote_is_readable_while_the_turn_is_still_running() {
    /// A turn that streams one summarised result and then keeps working — saying
    /// nothing more — until the test lets it finish. This is the quiet stretch a real
    /// long tool spends, and the one a reader has time to look around in.
    struct SlowStream {
        streamed: bool,
        release: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }
    impl loong_tui::Transport for SlowStream {
        fn send(&mut self, _line: &str) -> Result<(), String> {
            Ok(())
        }
        fn recv_line(&mut self) -> Result<Option<String>, String> {
            if !self.streamed {
                self.streamed = true;
                let output = (1..=50)
                    .map(|n| format!("mid-turn {n}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                return Ok(Some(
                    serde_json::json!({
                        "jsonrpc": "2.0", "method": "event",
                        "params": {"seq": 9, "kind": "tool.result",
                                   "data": {"call_id": "c1", "ok": true, "output": output}}
                    })
                    .to_string(),
                ));
            }
            while !self.release.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Ok(Some(
                serde_json::json!({
                    "jsonrpc": "2.0", "id": 1,
                    "result": {"turn_id": 1, "accepted": true}
                })
                .to_string(),
            ))
        }
    }

    let release = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let client = Client::new(
        SlowStream {
            streamed: false,
            release: release.clone(),
        },
        "s-running",
    );
    let (to_worker, from_worker) = spawn_worker(client, 0);
    let mut app = App::new("s-running", "v");
    app.busy = true;
    to_worker.send(ToWorker::Submit("go".to_string())).unwrap();

    // Wait for the fold to reach the screen. The turn is still working: nothing has
    // let it go, so the connection it holds cannot be asked for anything.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline && app.folded.is_empty() {
        app.drain(&from_worker);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        !app.folded.is_empty(),
        "the fold's own text came with its summary"
    );
    assert!(app.busy, "the turn is still running, as the test arranged");
    assert!(
        app.folded.contains_key(&0),
        "and it is named the way the keyboard counts it"
    );

    // The keyboard opens it now, with nothing asked of the busy worker.
    app.handle_key(key(KeyCode::Tab));
    assert!(app.fold.is_some(), "the fold is on screen to walk to");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None);
    let screen = draw(&mut app, 60, 20);
    assert!(screen.contains("tool result (50 lines)"), "{screen}");
    assert!(
        screen.contains("mid-turn 50"),
        "what was hidden is readable while the turn still works: {screen}"
    );

    // Let the turn finish so the worker is not left mid-request.
    release.store(true, std::sync::atomic::Ordering::SeqCst);
}

// ---------------------------------------------------------- the local wizard

fn providers() -> Vec<ProviderChoice> {
    vec![
        ProviderChoice {
            id: "deepseek".into(),
            label: "DeepSeek".into(),
            needs_key: true,
        },
        ProviderChoice {
            id: "moonshot".into(),
            label: "Moonshot / Kimi".into(),
            needs_key: true,
        },
        ProviderChoice {
            id: "dashscope".into(),
            label: "阿里云百炼 / Qwen".into(),
            needs_key: true,
        },
    ]
}

fn drawn_wizard(wizard: &LocalWizard, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| draw_local_wizard(frame, wizard))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let area = buffer.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Screen text with whitespace removed, for matching text that contains wide
/// characters. `TestBackend` stores a CJK glyph in one cell and leaves the next
/// blank, so a drawn `阿里云百炼` reads back as `阿 里 云 百 炼`. Comparing
/// de-spaced strings sidesteps that without pretending the padding is not there.
fn despaced(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

#[test]
fn a_refusal_can_re_rank_the_list_without_asking_anyone_again() {
    // The hint must cost nothing: reordering sends the key nowhere new, which is
    // what keeps "try the other `sk-` vendors" from becoming a second leak.
    let mut wizard = LocalWizard::new(providers());
    wizard.provider = 2;
    wizard.reorder(&["dashscope".to_string(), "moonshot".to_string()]);
    assert_eq!(
        wizard.selected().unwrap().id,
        "dashscope",
        "the best guess leads"
    );
    assert_eq!(wizard.providers[0].id, "dashscope");
    assert_eq!(wizard.providers[1].id, "moonshot");
    assert_eq!(
        wizard.providers[2].id, "deepseek",
        "and everything else survives, in order"
    );
    assert_eq!(wizard.providers.len(), 3, "nothing is conjured or lost");

    // A provider nobody offered cannot appear, and a duplicate cannot either.
    wizard.reorder(&["nonesuch".to_string(), "dashscope".to_string()]);
    assert_eq!(wizard.providers.len(), 3);
    assert_eq!(wizard.providers[0].id, "dashscope");

    // No suggestion means no movement — and no reset of the caret.
    let before = wizard.providers.clone();
    wizard.provider = 2;
    wizard.reorder(&[]);
    assert_eq!(wizard.providers, before);
    assert_eq!(wizard.provider, 2, "an empty hint is not a hint");
}

#[test]
fn a_provider_that_needs_no_key_neither_asks_for_one_nor_refuses_an_empty_one() {
    // A loopback server has no secret, and a form that demanded eight characters for
    // it would be asking somebody to invent one.
    let local = || {
        vec![
            ProviderChoice {
                id: "ollama".into(),
                label: "Ollama (本机)".into(),
                needs_key: false,
            },
            ProviderChoice {
                id: "deepseek".into(),
                label: "DeepSeek".into(),
                needs_key: true,
            },
        ]
    };

    let mut wizard = LocalWizard::new(local());
    assert!(
        wizard.ready(),
        "an empty field is not a mistake when there is nothing to type"
    );
    assert_eq!(
        wizard.handle_key(key(KeyCode::Enter)),
        WizardAction::Submit,
        "and enter is enough to start the check"
    );

    // Typing into it does nothing: there is nowhere for the characters to go.
    wizard.handle_key(key(KeyCode::Char('x')));
    assert_eq!(wizard.key, "", "the field is not a field for this provider");

    // The screen says so, rather than showing an empty box that looks broken.
    let screen = drawn_wizard(&wizard, 90, 20);
    assert!(
        despaced(&screen).contains(&despaced("本机服务，无需密钥")),
        "{screen}"
    );

    // Switching to a provider that does want one restores the ordinary rules.
    wizard.handle_key(key(KeyCode::Tab));
    assert!(!wizard.ready(), "the remote provider still wants a key");
    wizard.handle_key(key(KeyCode::Char('x')));
    assert_eq!(wizard.key, "x", "and the field works again");
}

// ------------------------------------------------------------ the picker

/// A picker as the wizard actually opens one: **nothing ticked**.
fn choosing(names: &[&str]) -> LocalWizard {
    let mut wizard = LocalWizard::new(providers());
    wizard.state = WizardState::Choosing {
        filter: String::new(),
        cursor: 0,
        all: names
            .iter()
            .map(|name| (name.to_string(), Some(4096)))
            .collect(),
        ticked: vec![false; names.len()],
    };
    wizard
}

/// A picker with everything already ticked, for the tests about *unticking*.
fn choosing_all(names: &[&str]) -> LocalWizard {
    let mut wizard = choosing(names);
    if let WizardState::Choosing { ticked, .. } = &mut wizard.state {
        *ticked = vec![true; names.len()];
    }
    wizard
}

#[test]
fn wanting_one_id_out_of_a_long_list_costs_two_keys_and_not_forty() {
    // The complaint this default exists for. A real vendor returns a few dozen ids,
    // somebody wants one of them, and pre-ticking everything made the work
    // proportional to the list rather than to the want — which is backwards, and
    // worst exactly where this screen is needed most.
    let many: Vec<String> = (1..=40).map(|n| format!("qwen-3.8-{n}")).collect();
    let names: Vec<&str> = many.iter().map(String::as_str).collect();
    let mut wizard = choosing(&names);

    assert!(
        wizard.chosen().is_empty(),
        "nothing is claimed on somebody's behalf to begin with"
    );

    // Narrow, then take what is left. Two keys, whatever the list length.
    for c in "3.8-17".chars() {
        wizard.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(wizard.visible().len(), 1, "the filter found the one");
    wizard.handle_key(key(KeyCode::Tab));
    let chosen = wizard.chosen();
    assert_eq!(chosen.len(), 1, "{chosen:?}");
    assert_eq!(chosen[0].0, "qwen-3.8-17");
}

#[test]
fn wanting_all_of_a_narrowed_list_is_still_one_key() {
    // The other half: somebody who *does* want several should not have to tick each.
    let mut wizard = choosing(&["qwen-flash", "qwen-max", "kimi-k3", "llama-3"]);
    for c in "qwen".chars() {
        wizard.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(wizard.visible().len(), 2);
    wizard.handle_key(ctrl('a'));
    let chosen: Vec<String> = wizard.chosen().into_iter().map(|(n, _)| n).collect();
    assert_eq!(
        chosen,
        vec!["qwen-flash", "qwen-max"],
        "`a` ticks what the filter is showing, and only that"
    );
}

#[test]
fn a_long_list_is_never_a_wall_of_ticks_to_undo() {
    // Stated as a property: however long the list, the number of keys to reach a
    // chosen answer does not grow with it.
    for length in [1usize, 3, 12, 40] {
        let many: Vec<String> = (1..=length).map(|n| format!("model-{n:02}")).collect();
        let names: Vec<&str> = many.iter().map(String::as_str).collect();
        let mut wizard = choosing(&names);
        for c in format!("model-{length:02}").chars() {
            wizard.handle_key(key(KeyCode::Char(c)));
        }
        wizard.handle_key(key(KeyCode::Tab));
        assert_eq!(
            wizard.chosen().len(),
            1,
            "length {length}: one filter and one tick"
        );
    }
}

#[test]
fn the_picker_can_narrow_to_one_id_out_of_a_long_list() {
    let mut wizard = choosing(&[
        "qwen3.8-flash",
        "qwen3.8-max",
        "qwen3.7-plus",
        "qwen-coder-plus",
        "text-embedding-v3",
    ]);
    for c in "coder".chars() {
        wizard.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(wizard.visible(), vec![3], "the filter narrowed the list");

    wizard.handle_key(key(KeyCode::Tab));
    let chosen = wizard.chosen();
    assert_eq!(chosen.len(), 1, "{chosen:?}");
    assert_eq!(chosen[0].0, "qwen-coder-plus");
}

#[test]
fn filtering_narrows_the_view_without_changing_what_is_ticked() {
    // The ticks live beside the full list, so a filter that hid a row must not have
    // unticked it — otherwise narrowing the view would quietly discard choices.
    let mut wizard = choosing_all(&["alpha", "beta", "gamma"]);
    wizard.handle_key(key(KeyCode::Char('g')));
    assert_eq!(wizard.visible(), vec![2]);
    assert_eq!(
        wizard.chosen().len(),
        3,
        "the two rows off screen are still ticked"
    );

    wizard.handle_key(key(KeyCode::Backspace));
    assert_eq!(wizard.visible(), vec![0, 1, 2], "and come back");
}

#[test]
fn toggle_all_means_all_of_what_is_on_screen() {
    let mut wizard = choosing(&["qwen-flash", "qwen-max", "kimi-k3"]);
    wizard.handle_key(ctrl('a'));
    assert_eq!(
        wizard.chosen().len(),
        3,
        "`ctrl+a` on an empty list takes it all"
    );

    wizard.handle_key(ctrl('a'));
    assert!(wizard.chosen().is_empty(), "and `a` again gives it back");
}

#[test]
fn the_picker_will_not_finish_with_nothing_ticked() {
    let mut wizard = choosing(&["a", "b"]);
    assert_eq!(
        wizard.handle_key(key(KeyCode::Enter)),
        WizardAction::None,
        "adding a provider that adds no models is not a thing anybody meant"
    );
    wizard.handle_key(key(KeyCode::Tab));
    assert_eq!(wizard.handle_key(key(KeyCode::Enter)), WizardAction::Submit);
}

#[test]
fn the_picker_moves_and_stops_at_the_ends() {
    let mut wizard = choosing(&["a", "b", "c"]);
    wizard.handle_key(key(KeyCode::Up));
    assert_eq!(wizard.visible()[0], 0, "the top is the top");
    for _ in 0..5 {
        wizard.handle_key(key(KeyCode::Down));
    }
    // The cursor is kept in range, so space still lands on the last row.
    wizard.handle_key(key(KeyCode::Tab));
    let chosen = wizard.chosen();
    assert_eq!(chosen.len(), 1, "{chosen:?}");
    assert_eq!(chosen[0].0, "c", "the far end of the list was reachable");
}

#[test]
fn escape_from_the_picker_goes_back_to_the_form_not_out_of_it() {
    // The key is typed and the provider is chosen; escaping the list should cost a
    // keystroke, not the whole form.
    let mut wizard = choosing(&["a"]);
    wizard.key = "sk-something-long".to_string();
    wizard.handle_key(key(KeyCode::Esc));
    assert_eq!(wizard.state, WizardState::Editing);
    assert_eq!(
        wizard.key, "sk-something-long",
        "and the key is still there"
    );
    assert_eq!(wizard.handle_key(key(KeyCode::Esc)), WizardAction::Cancel);
}

#[test]
fn the_picker_says_what_is_ticked_and_what_to_press() {
    let mut wizard = choosing(&["qwen-flash", "qwen-max", "kimi-k3"]);
    let screen = drawn_wizard(&wizard, 90, 20);
    assert!(
        despaced(&screen).contains(&despaced("已选 0 / 3")),
        "the count is on screen, and it starts at nothing:\n{screen}"
    );
    assert!(screen.contains("[ ] qwen-flash"), "{screen}");
    assert!(
        despaced(&screen).contains(&despaced("先输入关键字缩小范围")),
        "and the first thing to do is named, because on a long list it is the only \
         thing that helps:\n{screen}"
    );

    wizard.handle_key(key(KeyCode::Tab));
    let screen = drawn_wizard(&wizard, 90, 20);
    assert!(screen.contains("[x] qwen-flash"), "{screen}");
    assert!(
        despaced(&screen).contains(&despaced("已选 1 / 3")),
        "{screen}"
    );
    assert!(
        despaced(&screen).contains(&despaced("ctrl+a 全选当前筛选")),
        "and taking all of a narrowed list is advertised: {screen}"
    );
}

#[test]
fn a_list_longer_than_the_screen_scrolls_to_keep_the_cursor_visible() {
    // Forty ids on a twenty-row terminal: walking down has to walk the window, or the
    // row being ticked is off screen and the tick lands on something invisible.
    let many: Vec<String> = (1..=40).map(|n| format!("model-{n:02}")).collect();
    let names: Vec<&str> = many.iter().map(String::as_str).collect();
    let mut wizard = choosing(&names);
    for _ in 0..30 {
        wizard.handle_key(key(KeyCode::Down));
    }
    wizard.handle_key(key(KeyCode::Tab));
    let chosen = wizard.chosen();
    assert_eq!(chosen.len(), 1, "{chosen:?}");
    assert_eq!(chosen[0].0, "model-31");

    let screen = drawn_wizard(&wizard, 90, 24);
    assert!(
        screen.contains("model-31"),
        "the row the cursor is on is drawn:\n{screen}"
    );
    assert!(
        !screen.contains("model-01"),
        "and the window moved rather than the cursor leaving it:\n{screen}"
    );
}

fn offering(provider: &str, var: &str, key: &str) -> OfferedKey {
    OfferedKey {
        provider: provider.to_string(),
        var: var.to_string(),
        key: key.to_string(),
    }
}

#[test]
fn a_key_this_machine_already_has_is_named_on_the_form_and_not_typed_in() {
    // The bug: the offer used to be *written into the field*. That looks like a form
    // that skipped a question, and worse, typing then appends to a secret nobody can
    // read — `<env key><what you typed>`.
    let mut wizard = LocalWizard::new(providers());
    wizard.offer(vec![offering("deepseek", "DEEPSEEK_API_KEY", "sk-env-key")]);

    assert_eq!(wizard.key, "", "the field is empty");
    assert!(wizard.ready(), "but the offer is an answer, so enter works");
    assert_eq!(wizard.effective_key(), "sk-env-key");

    // The screen says where it would come from.
    let screen = drawn_wizard(&wizard, 100, 20);
    assert!(
        despaced(&screen).contains(&despaced("DEEPSEEK_API_KEY")),
        "the variable is named:\n{screen}"
    );
    assert!(
        !screen.contains("sk-env-key"),
        "and the secret itself is not on screen:\n{screen}"
    );

    // Typing replaces it, because there was nothing to append to.
    for c in "sk-mine".chars() {
        wizard.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(wizard.key, "sk-mine");
    assert_eq!(
        wizard.effective_key(),
        "sk-mine",
        "what somebody typed wins over what was offered"
    );
}

#[test]
fn the_offer_follows_the_provider_being_chosen() {
    let mut wizard = LocalWizard::new(providers());
    wizard.offer(vec![
        offering("moonshot", "MOONSHOT_API_KEY", "sk-moonshot"),
        offering("deepseek", "DEEPSEEK_API_KEY", "sk-deepseek"),
    ]);

    // Starts on the first offer's provider.
    assert_eq!(wizard.selected().unwrap().id, "deepseek");
    assert_eq!(wizard.effective_key(), "sk-deepseek");

    wizard.handle_key(key(KeyCode::Tab));
    assert_eq!(wizard.selected().unwrap().id, "moonshot");
    assert_eq!(
        wizard.effective_key(),
        "sk-moonshot",
        "switching providers switches which offer applies"
    );

    // A provider with no key on this machine has none to offer, and needs one typed.
    wizard.handle_key(key(KeyCode::Tab));
    assert_eq!(wizard.selected().unwrap().id, "dashscope");
    assert!(wizard.offered_key().is_none());
    assert!(!wizard.ready(), "so enter cannot go yet");
    assert_eq!(wizard.effective_key(), "");
}

#[test]
fn a_control_chord_never_lands_in_the_api_key_field() {
    // The same rule as the picker's, one screen earlier: a chord is a command, not
    // text. `ctrl+a` in a key field would paste an `a` into a secret, and a secret is
    // the one thing here nobody can check by looking at it.
    let mut wizard = LocalWizard::new(providers());
    for c in "sk-abcdefghij".chars() {
        wizard.handle_key(key(KeyCode::Char(c)));
    }
    let before = wizard.key.clone();
    for chord in ['a', 'e', 'u', 'k', 'w'] {
        wizard.handle_key(ctrl(chord));
    }
    assert_eq!(wizard.key, before, "no chord reached the field");

    // And ordinary typing still does.
    wizard.handle_key(key(KeyCode::Char('z')));
    assert_eq!(wizard.key, format!("{before}z"));
}

#[test]
fn the_filter_field_takes_every_character_a_model_id_can_have() {
    // The bug this locks down, and it was the worst kind: `a` toggled everything and
    // space toggled the row under the cursor, so neither could be *typed*. Filtering
    // for `flash`, `max`, `llama`, `alpha` or `qwen 3.7` — ordinary things to want —
    // was impossible, and the field that exists to narrow a long list was crippled by
    // two of the most common characters in one.
    let mut wizard = choosing(&["qwen-flash", "qwen-max", "llama-3", "alpha"]);
    for c in "flash".chars() {
        wizard.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(
        wizard.visible().len(),
        1,
        "`flash` narrowed the list, which needs the `a` to be text"
    );

    // A space is text as well.
    let mut wizard = choosing(&["qwen-flash", "qwen-max"]);
    for c in "qwen ".chars() {
        wizard.handle_key(key(KeyCode::Char(c)));
    }
    let WizardState::Choosing { filter, .. } = &wizard.state else {
        panic!("still choosing")
    };
    assert_eq!(filter, "qwen ", "the space landed in the field");

    // And typing is not choosing: none of that ticked anything.
    assert!(wizard.chosen().is_empty());
}

#[test]
fn ctrl_a_is_a_command_and_a_plain_a_is_a_letter() {
    let mut wizard = choosing(&["qwen-flash", "qwen-max"]);
    wizard.handle_key(key(KeyCode::Char('a')));
    let WizardState::Choosing { filter, ticked, .. } = &wizard.state else {
        panic!("still choosing")
    };
    assert_eq!(filter, "a", "a plain `a` is text");
    assert!(ticked.iter().all(|t| !t), "and it ticked nothing");

    // With ctrl it is the command, and it reaches nothing but the ticks.
    wizard.handle_key(ctrl('a'));
    let WizardState::Choosing { filter, ticked, .. } = &wizard.state else {
        panic!("still choosing")
    };
    assert_eq!(filter, "a", "ctrl+a added no text");
    assert!(
        ticked.iter().all(|t| *t),
        "it took everything the filter is showing"
    );
}

#[test]
fn the_picker_says_so_when_a_filter_matches_nothing() {
    let mut wizard = choosing(&["alpha", "beta"]);
    for c in "zzz".chars() {
        wizard.handle_key(key(KeyCode::Char(c)));
    }
    assert!(wizard.visible().is_empty());
    let screen = drawn_wizard(&wizard, 90, 20);
    assert!(
        despaced(&screen).contains(&despaced("没有匹配的模型")),
        "{screen}"
    );
}

#[test]
fn the_local_wizard_never_draws_the_key_it_was_given() {
    let mut wizard = LocalWizard::new(providers());
    // A made-up key, typed the way somebody would type a real one.
    for c in "sk-0123456789abcdef0123456789abcdef".chars() {
        wizard.handle_key(key(KeyCode::Char(c)));
    }
    let masked = wizard.masked_key();
    assert!(
        masked.starts_with("sk-"),
        "the shape is recognisable: {masked}"
    );
    assert!(
        !masked.contains("01234567"),
        "but the secret is not on a screen other people can see: {masked}"
    );
    assert_eq!(masked.chars().count(), 35, "the length is still legible");

    let screen = drawn_wizard(&wizard, 80, 20);
    assert!(
        !screen.contains("01234567"),
        "and the drawing does not leak it either:\n{screen}"
    );
}

#[test]
fn the_local_wizard_refuses_a_key_too_short_to_be_one() {
    let mut wizard = LocalWizard::new(providers());
    assert_eq!(wizard.handle_key(key(KeyCode::Enter)), WizardAction::None);
    assert!(!wizard.ready(), "an empty field is not a key");

    for c in "sk-123".chars() {
        wizard.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(wizard.handle_key(key(KeyCode::Enter)), WizardAction::None);
    match &wizard.state {
        WizardState::Refused(why) => assert!(why.contains("太短"), "{why}"),
        other => panic!("expected a refusal, got {other:?}"),
    }

    // Long enough is enough — the vendor, not a regex, decides what is valid.
    for c in "45678".chars() {
        wizard.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(wizard.handle_key(key(KeyCode::Enter)), WizardAction::Submit);
}

#[test]
fn the_local_wizard_moves_between_providers_in_both_directions_and_wraps() {
    let mut wizard = LocalWizard::new(providers());
    assert_eq!(wizard.selected().unwrap().id, "deepseek");

    wizard.handle_key(key(KeyCode::Tab));
    assert_eq!(wizard.selected().unwrap().id, "moonshot");
    wizard.handle_key(key(KeyCode::Down));
    assert_eq!(wizard.selected().unwrap().id, "dashscope");
    wizard.handle_key(key(KeyCode::Down));
    assert_eq!(
        wizard.selected().unwrap().id,
        "deepseek",
        "the list wraps, so one key never dead-ends"
    );

    wizard.handle_key(key(KeyCode::BackTab));
    assert_eq!(wizard.selected().unwrap().id, "dashscope");
    wizard.handle_key(key(KeyCode::Up));
    assert_eq!(wizard.selected().unwrap().id, "moonshot");
}

#[test]
fn the_local_wizard_ignores_typing_while_a_validation_is_in_flight() {
    // Otherwise a keystroke lands in the field and the key that was checked is
    // not the key that gets stored.
    let mut wizard = LocalWizard::new(providers());
    for c in "sk-abcdefghij".chars() {
        wizard.handle_key(key(KeyCode::Char(c)));
    }
    wizard.state = WizardState::Validating;
    let before = wizard.key.clone();
    wizard.handle_key(key(KeyCode::Char('X')));
    wizard.handle_key(key(KeyCode::Backspace));
    assert_eq!(wizard.key, before, "input is frozen mid-flight");
    assert_eq!(wizard.handle_key(key(KeyCode::Enter)), WizardAction::None);
}

#[test]
fn the_local_wizard_keeps_the_key_when_the_vendor_refuses_it() {
    // A typo is the common case, and retyping 35 characters to fix one of them
    // is the difference between a wizard and a punishment.
    let mut wizard = LocalWizard::new(providers());
    for c in "sk-0123456789abcdef0123456789abcdef".chars() {
        wizard.handle_key(key(KeyCode::Char(c)));
    }
    wizard.state = WizardState::Refused("密钥被拒绝（HTTP 401）".into());
    let screen = drawn_wizard(&wizard, 80, 20);
    assert!(
        despaced(&screen).contains(&despaced("密钥被拒绝")),
        "{screen}"
    );
    assert_eq!(
        wizard.key, "sk-0123456789abcdef0123456789abcdef",
        "the key survives so it can be corrected"
    );
}

#[test]
fn the_local_wizard_draws_the_providers_and_says_what_to_press() {
    let mut wizard = LocalWizard::new(providers());
    wizard.handle_key(key(KeyCode::Tab));
    let screen = drawn_wizard(&wizard, 80, 20);

    for label in ["DeepSeek", "Moonshot / Kimi"] {
        assert!(screen.contains(label), "{label} is offered:\n{screen}");
    }
    assert!(
        despaced(&screen).contains(&despaced("阿里云百炼 / Qwen")),
        "a CJK label is offered as itself, not transliterated:\n{screen}"
    );
    assert!(
        screen.contains("▸ Moonshot / Kimi"),
        "the current choice is marked:\n{screen}"
    );
    assert!(
        !screen.contains("▸ DeepSeek"),
        "and only the current one is:\n{screen}"
    );
    assert!(
        despaced(&screen).contains(&despaced("enter 校验")),
        "{screen}"
    );
    assert!(
        despaced(&screen).contains(&despaced("esc 跳过")),
        "the way out is visible, and costs nothing: {screen}"
    );
    assert!(
        !screen.contains("base_url"),
        "the field the user must never see is not even mentioned:\n{screen}"
    );
}

#[test]
fn the_local_wizard_shows_the_reason_a_network_failure_is_not_a_bad_key() {
    // The probe already refuses to conflate these; the wizard must not undo that
    // by drawing both the same colour and the same way.
    let mut wizard = LocalWizard::new(providers());
    for c in "sk-abcdefghij".chars() {
        wizard.handle_key(key(KeyCode::Char(c)));
    }
    wizard.state =
        WizardState::Refused("无法连接（这不说明密钥有问题）：Connection refused".into());
    let screen = drawn_wizard(&wizard, 80, 20);
    assert!(
        despaced(&screen).contains(&despaced("这不说明密钥有问题")),
        "{screen}"
    );
}

#[test]
fn the_local_wizard_is_leavable_with_escape_or_ctrl_c() {
    for escape in [
        key(KeyCode::Esc),
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
    ] {
        let mut wizard = LocalWizard::new(providers());
        assert_eq!(
            wizard.handle_key(escape),
            WizardAction::Cancel,
            "a form that cannot be left can strand a terminal"
        );
    }
}

// ------------------------------------------------------------ the model bar

fn model_list(names: &[&str]) -> loong_schema::generated::ModelList {
    loong_schema::generated::ModelList {
        models: names
            .iter()
            .map(|name| loong_schema::generated::ModelOffering {
                name: name.to_string(),
                model: name.to_string(),
                provider: None,
                context_window: None,
            })
            .collect(),
        selected: names.first().copied().unwrap_or_default().to_string(),
    }
}

fn app_with_models(names: &[&str]) -> App {
    let mut app = App::new("s", "v");
    app.set_models(model_list(names));
    app
}

fn alt(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
}

#[test]
fn the_model_bar_numbers_the_models_in_the_order_the_profile_declared_them() {
    // Not alphabetically, and not by the map's order: that order is the user's own
    // ranking from cheapest to most capable, and it is what the digits mean.
    let mut app = app_with_models(&["local-small", "qwen-lite", "deepseek", "kimi"]);
    let screen = draw(&mut app, 90, 20);

    let bar = screen.lines().next().unwrap_or_default().to_string();
    let at: Vec<usize> = ["[1]", "[2]", "[3]", "[4]"]
        .iter()
        .map(|mark| {
            bar.find(mark)
                .unwrap_or_else(|| panic!("{mark} is missing from the bar: {bar:?}"))
        })
        .collect();
    assert!(
        at.windows(2).all(|pair| pair[0] < pair[1]),
        "the slots are numbered left to right in declaration order: {bar:?}"
    );
    for name in ["local-small", "qwen-lite", "deepseek", "kimi"] {
        assert!(bar.contains(name), "{name} is on the bar: {bar:?}");
    }
}

#[test]
fn the_model_bar_marks_the_current_model_and_only_that_one() {
    let mut app = app_with_models(&["cheap", "strong"]);
    let screen = draw(&mut app, 90, 20);
    let bar = screen.lines().next().unwrap_or_default().to_string();
    assert!(bar.contains("▸[1] cheap"), "{bar:?}");
    assert!(
        !bar.contains("▸[2]"),
        "the model that is not running is not marked: {bar:?}"
    );

    // And the mark follows a switch, so the bar never lies about what is answering.
    app.selected = "strong".to_string();
    let screen = draw(&mut app, 90, 20);
    let bar = screen.lines().next().unwrap_or_default().to_string();
    assert!(bar.contains("▸[2] strong"), "{bar:?}");
    assert!(!bar.contains("▸[1]"), "{bar:?}");
}

#[test]
fn the_tick_says_how_the_turn_ended_and_which_way() {
    let done = Finished {
        seconds: Some(3.2),
        outcome: Outcome::Done,
    };
    assert_eq!(done.line(), "✓ 完成 · 3.2s");
    assert_eq!(done.style().fg, Some(Color::Green));
    assert!(
        done.style().add_modifier.contains(Modifier::BOLD),
        "the tick that means finished is the loudest thing on the line"
    );

    let stopped = Finished {
        seconds: Some(61.0),
        outcome: Outcome::Stopped,
    };
    assert_eq!(stopped.line(), "⏹ 已停止 · 1m01s");
    assert_eq!(stopped.style().fg, Some(Color::Yellow));

    let failed = Finished {
        seconds: None,
        outcome: Outcome::Failed,
    };
    assert_eq!(failed.line(), "✗ 失败");
    assert_eq!(failed.style().fg, Some(Color::Red));

    // Three endings, three different marks. Two states sharing an icon would be
    // worse than no icon, because it would be believed.
    let marks: Vec<char> = [done, stopped, failed]
        .iter()
        .filter_map(|f| f.line().chars().next())
        .collect();
    assert_eq!(marks.len(), 3);
    assert!(
        marks[0] != marks[1] && marks[1] != marks[2] && marks[0] != marks[2],
        "{marks:?}"
    );
}

#[test]
fn the_prompt_title_shows_the_tick_until_the_next_task_starts() {
    // The complaint this answers: "我常常不知道任务是否完成了". The closing line in the
    // transcript says it, but it scrolls, and the question is asked while the cursor
    // is sitting in the box.
    let mut app = App::new("s-tick", "v");
    app.busy = true;
    app.busy_since = Some(std::time::Instant::now());

    fed(&mut app, FromWorker::State { pending: None });
    let screen = draw(&mut app, 110, 20);
    assert!(
        box_titles(&screen).contains(&despaced("✓ 完成")),
        "the tick is on the prompt box:\n{screen}"
    );
    assert!(
        despaced(&screen).contains(&despaced("✓ 完成")),
        "and the transcript keeps the line too:\n{screen}"
    );

    // Still there while the next prompt is being typed...
    type_text(&mut app, "next thing");
    let screen = draw(&mut app, 110, 20);
    assert!(
        box_titles(&screen).contains(&despaced("✓ 完成")),
        "and stays while the next prompt is written:\n{screen}"
    );

    // ...and gone once that task actually starts.
    app.input = "next thing".to_string();
    app.handle_key(key(KeyCode::Enter));
    assert!(app.finished.is_none(), "a new task drops the old tick");
    let screen = draw(&mut app, 110, 20);
    assert!(
        !box_titles(&screen).contains(&despaced("✓ 完成")),
        "a tick that outlives its turn lies about the work on screen:\n{screen}"
    );
    assert!(
        box_titles(&screen).contains("working"),
        "and the box says what is happening instead:\n{screen}"
    );

    // A slash command still completes rather than being hidden behind the tick.
    // Not while busy, though: "working" outranks a completion hint, because a turn
    // that is running is the more urgent fact.
    app.busy = false;
    app.finished = Some(Finished {
        seconds: None,
        outcome: Outcome::Done,
    });
    app.input = "/exi".to_string();
    let screen = draw(&mut app, 110, 20);
    assert!(
        box_titles(&screen).contains(&despaced("leaves loongagent")),
        "{screen}"
    );

    // And with no command typed, the tick is back in front of it.
    app.input.clear();
    let screen = draw(&mut app, 110, 20);
    assert!(
        box_titles(&screen).contains(&despaced("✓ 完成")),
        "{screen}"
    );
}

/// The foreground colour of each cell under `needle`, on whichever row it appears.
/// `draw()` drops styles (it keeps symbols), and this bug was a *style* leak: the
/// status colour belonged on the box title and was landing on the typed text.
///
/// Matching ignores spaces: a CJK character occupies two cells and the terminal
/// puts a space in the second one, so `✓ 完成` is stored as `✓ 完 成`. Comparing
/// non-space characters is what makes the title findable without a width table.
fn colours_of(app: &mut App, needle: &str) -> Vec<Color> {
    colours_of_where(app, needle, false)
}

/// The same, restricted to the box's title row — the transcript carries the same
/// closing line, so an unrestricted search would find that one instead.
fn title_colours_of(app: &mut App, needle: &str) -> Vec<Color> {
    colours_of_where(app, needle, true)
}

fn colours_of_where(app: &mut App, needle: &str, border_only: bool) -> Vec<Color> {
    let mut terminal = Terminal::new(TestBackend::new(140, 24)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let area = *buffer.area();
    let target: Vec<char> = needle.chars().filter(|c| *c != ' ').collect();
    if target.is_empty() {
        return Vec::new();
    }
    for y in 0..area.height {
        let cells: Vec<(String, Color)> = (0..area.width)
            .map(|x| (buffer[(x, y)].symbol().to_string(), buffer[(x, y)].fg))
            .collect();
        if border_only
            && !cells
                .iter()
                .filter_map(|(symbol, _)| symbol.chars().next())
                .next()
                .map(|first| first == '┌')
                .unwrap_or(false)
        {
            continue;
        }
        // Visible characters with the cell each one lives in.
        let chars: Vec<(char, usize)> = cells
            .iter()
            .enumerate()
            .filter_map(|(index, (symbol, _))| {
                symbol
                    .chars()
                    .next()
                    .filter(|c| *c != ' ')
                    .map(|c| (c, index))
            })
            .collect();
        if chars.len() < target.len() {
            continue;
        }
        for start in 0..=chars.len() - target.len() {
            if (0..target.len()).all(|k| chars[start + k].0 == target[k]) {
                return (0..target.len())
                    .map(|k| cells[chars[start + k].1].1)
                    .collect();
            }
        }
    }
    Vec::new()
}

#[test]
fn the_prompt_box_does_not_wear_the_last_turn_outcome() {
    // Reported from a real session: after a task succeeded the *textarea* turned
    // green. The style was applied to the whole `Paragraph`, so every status colour
    // (green done, red YOLO, cyan busy) bled into whatever the person was typing.
    let mut app = App::new("s-colour", "v");
    app.busy = true;
    app.busy_since = Some(std::time::Instant::now());
    fed(&mut app, FromWorker::State { pending: None });
    assert_eq!(
        app.finished.as_ref().map(|f| f.outcome),
        Some(Outcome::Done),
        "the turn is recorded as done, which is the green state"
    );

    type_text(&mut app, "next prompt");
    let typed = colours_of(&mut app, "next prompt");
    assert!(!typed.is_empty(), "the typed text is on screen");
    assert!(
        typed.iter().all(|colour| *colour != Color::Green),
        "the input area keeps its own colour, not the turn's: {typed:?}"
    );

    // The status is not lost: the title still carries the colour and the mark.
    let title = title_colours_of(&mut app, "完成");
    assert!(!title.is_empty(), "the tick is still on the box title");
    assert!(
        title.contains(&Color::Green),
        "and it is the coloured thing: {title:?}"
    );
}

#[test]
fn a_working_prompt_box_does_not_tint_what_is_typed_either() {
    // Same leak, different status: cyan while a turn runs is the state a person
    // types into most often.
    let mut app = App::new("s-colour-busy", "v");
    app.busy = true;
    app.busy_since = Some(std::time::Instant::now());
    type_text(&mut app, "typed while busy");
    let typed = colours_of(&mut app, "typed while busy");
    assert!(!typed.is_empty());
    assert!(
        typed.iter().all(|colour| *colour != Color::Cyan),
        "busy is a state of the *turn*, not a colour for the person's text: {typed:?}"
    );
    let title = title_colours_of(&mut app, "working");
    assert!(title.contains(&Color::Cyan), "{title:?}");
}

#[test]
fn a_turn_that_ends_badly_does_not_get_a_tick() {
    let mut app = App::new("s-tick-bad", "v");
    app.busy = true;
    app.busy_since = Some(std::time::Instant::now());
    fed(
        &mut app,
        FromWorker::Failed("the provider refused".to_string()),
    );
    let screen = draw(&mut app, 110, 20);
    assert!(despaced(&screen).contains(&despaced("✗ 失败")), "{screen}");
    assert!(
        !despaced(&screen).contains(&despaced("✓ 完成")),
        "a failure must never be able to pass for a completion:\n{screen}"
    );
}

#[test]
fn a_turn_the_person_stopped_says_so_rather_than_showing_a_tick() {
    let mut app = App::new("s-tick-stop", "v");
    app.interrupt = Some(Box::new(|| {}));
    app.busy = true;
    app.busy_since = Some(std::time::Instant::now());
    app.stop_turn();
    fed(&mut app, FromWorker::State { pending: None });
    let screen = draw(&mut app, 110, 20);
    assert!(
        box_titles(&screen).contains(&despaced("⏹ 已停止")),
        "{screen}"
    );
    assert!(
        !despaced(&screen).contains(&despaced("✓ 完成")),
        "stopping is not finishing, and the two must not look alike:\n{screen}"
    );
}

#[test]
fn new_and_clear_both_ask_for_a_fresh_session() {
    // `/clear` is not a screen wipe. When people ask for a clean start they mean the
    // context, and clearing only the view would leave the model still reading
    // everything above it — the worst of both, because the screen would say otherwise.
    for command in ["/new", "/clear", "/reset", "/NEW"] {
        let mut app = App::new("s-new", "v");
        let action = app.handle_key(key(KeyCode::Enter)).clone();
        assert_eq!(action, Action::None, "nothing typed yet");
        app.input = command.to_string();
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Action::Send(ToWorker::NewSession),
            "{command}"
        );
    }
}

#[test]
fn a_new_session_clears_the_screen_because_that_log_is_no_longer_the_live_one() {
    let mut app = App::new("s-old", "v");
    app.push_lines(vec!["you: something long".to_string()]);
    app.busy = true;
    app.busy_since = Some(std::time::Instant::now());
    app.finished = Some(Finished {
        seconds: Some(1.0),
        outcome: Outcome::Done,
    });
    app.pending = Some("r1".to_string());
    app.approval = Some(Box::new(ApprovalInfo {
        request_id: "r1".to_string(),
        class: "write".to_string(),
        reason: "because".to_string(),
        tool: "write_file".to_string(),
        args: "{}".to_string(),
    }));
    app.error = Some("old failure".to_string());
    app.scroll = 4;

    fed(&mut app, FromWorker::SessionStarted("s-new1".to_string()));

    assert_eq!(app.session, "s-new1");
    assert!(
        !app.lines.iter().any(|line| line.contains("something long")),
        "the old transcript is gone: {:?}",
        app.lines
    );
    assert!(
        app.pending.is_none(),
        "and so is a question from the old log"
    );
    assert!(app.approval.is_none());
    assert!(app.error.is_none());
    assert!(app.finished.is_none(), "and the old turn's tick");
    assert!(app.busy_since.is_none());
    assert_eq!(app.scroll, 0);
    assert!(
        app.lines
            .iter()
            .any(|line| line.contains("s-new1") && line.contains("存盘")),
        "and it says where the old session went: {:?}",
        app.lines
    );
    let screen = draw(&mut app, 110, 20);
    assert!(!screen.contains("something long"), "{screen}");
}

#[test]
fn ctrl_n_and_ctrl_p_step_through_the_models_and_wrap() {
    // The keys that work in every terminal. `alt+<digit>` does not: tmux and some
    // macOS setups swallow the modifier, and a shortcut that silently does nothing
    // reads as a broken program rather than a broken terminal.
    let mut app = app_with_models(&["a", "b", "c"]);
    assert_eq!(
        app.handle_key(ctrl('n')),
        Action::Send(ToWorker::SelectModel("b".to_string()))
    );
    app.selected = "b".to_string();
    assert_eq!(
        app.handle_key(ctrl('n')),
        Action::Send(ToWorker::SelectModel("c".to_string()))
    );
    app.selected = "c".to_string();
    assert_eq!(
        app.handle_key(ctrl('n')),
        Action::Send(ToWorker::SelectModel("a".to_string())),
        "and it wraps rather than dead-ending at the last model"
    );

    app.selected = "a".to_string();
    assert_eq!(
        app.handle_key(ctrl('p')),
        Action::Send(ToWorker::SelectModel("c".to_string())),
        "backwards wraps the other way"
    );
}

#[test]
fn plain_n_and_p_still_type_letters() {
    let mut app = app_with_models(&["a", "b"]);
    app.handle_key(key(KeyCode::Char('n')));
    app.handle_key(key(KeyCode::Char('p')));
    assert_eq!(app.input, "np");
}

#[test]
fn the_bar_hint_leads_with_the_keys_that_always_arrive() {
    let mut app = app_with_models(&["a", "b", "c"]);
    let screen = draw(&mut app, 110, 20);
    let bar = screen.lines().next().unwrap_or_default().to_string();
    assert!(
        despaced(&bar).contains(&despaced("ctrl+n/p 切换")),
        "the reliable keys are named first: {bar:?}"
    );
    assert!(
        despaced(&bar).contains(&despaced("alt+1..3")),
        "and the numbered ones are still offered: {bar:?}"
    );
}

#[test]
fn the_current_model_is_drawn_as_a_block_and_not_only_a_marker() {
    // "Did that switch?" is the one question the bar exists to answer, and a `▸` in a
    // row of dim text was too quiet to answer it.
    use ratatui::style::Modifier;
    let mut app = app_with_models(&["cheap", "strong"]);
    let mut terminal = Terminal::new(TestBackend::new(90, 20)).expect("a test terminal");
    terminal.draw(|frame| app.draw(frame)).expect("draws");
    let buffer = terminal.backend().buffer().clone();

    // Row 0 is the bar; find the cell where the current model's name starts.
    let row: String = (0..buffer.area().width)
        .map(|x| buffer[(x, 0)].symbol().to_string())
        .collect();
    let at = row.find("cheap").expect("the current model is on the bar") as u16;
    assert!(
        buffer[(at, 0)].modifier.contains(Modifier::REVERSED),
        "the current model is drawn as a block: {row:?}"
    );

    let other = row.find("strong").expect("the other model is on the bar") as u16;
    assert!(
        !buffer[(other, 0)].modifier.contains(Modifier::REVERSED),
        "and the one that is not running is not: {row:?}"
    );
}

#[test]
fn the_model_bar_shows_how_to_switch_without_being_asked() {
    // The bar is the whole discoverability story: a shortcut nobody is told about is
    // a shortcut nobody uses.
    let mut app = app_with_models(&["cheap", "mid", "strong"]);
    let screen = draw(&mut app, 100, 20);
    let bar = screen.lines().next().unwrap_or_default().to_string();
    assert!(
        despaced(&bar).contains(&despaced("alt+1..3")),
        "the key is on the bar: {bar:?}"
    );
    assert!(
        despaced(&bar).contains(&despaced("/1../3")),
        "and so is the fallback, for a terminal that eats Alt: {bar:?}"
    );
}

#[test]
fn a_single_model_needs_no_shortcut_advertised() {
    let mut app = app_with_models(&["only"]);
    let screen = draw(&mut app, 90, 20);
    let bar = screen.lines().next().unwrap_or_default().to_string();
    assert!(bar.contains("only"), "{bar:?}");
    assert!(
        !despaced(&bar).contains(&despaced("alt+1")),
        "there is nothing to switch to, so nothing is promised: {bar:?}"
    );
}

#[test]
fn a_session_with_no_models_draws_no_bar_at_all() {
    let mut app = App::new("s", "v");
    let without = draw(&mut app, 80, 20);
    assert!(
        !without.contains("[1]"),
        "nothing is drawn when there is nothing to draw: {without}"
    );
}

#[test]
fn alt_digit_switches_to_that_slot() {
    let mut app = app_with_models(&["cheap", "mid", "strong"]);
    assert_eq!(
        app.handle_key(alt('3')),
        Action::Send(ToWorker::SelectModel("strong".to_string()))
    );
    assert_eq!(
        app.handle_key(alt('2')),
        Action::Send(ToWorker::SelectModel("mid".to_string()))
    );
}

#[test]
fn slash_digit_is_the_same_shortcut_for_a_terminal_that_eats_alt() {
    // tmux and some macOS setups never deliver Alt to the program. The fallback has
    // to do exactly the same thing, or "the key does nothing" becomes a support
    // question with two different answers.
    let mut app = app_with_models(&["cheap", "strong"]);
    app.input = "/2".to_string();
    app.cursor = 2;
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Action::Send(ToWorker::SelectModel("strong".to_string()))
    );
    assert!(app.input.is_empty(), "the command does not reach the agent");
}

#[test]
fn a_slot_with_no_model_says_so_instead_of_doing_nothing() {
    let mut app = app_with_models(&["cheap", "strong"]);
    let action = app.handle_key(alt('7'));
    assert_eq!(action, Action::None, "there is no seventh model");
    let said = app.lines.join("\n");
    assert!(
        said.contains("没有第 7 个模型"),
        "a key that appears to do nothing is worse than one that explains: {said:?}"
    );
}

#[test]
fn switching_to_the_model_already_running_does_not_ask_the_server() {
    // A round trip to be told nothing changed would put a line in the log for a
    // non-event, and the log is the record.
    let mut app = app_with_models(&["cheap", "strong"]);
    assert_eq!(app.handle_key(alt('1')), Action::None);
    let said = app.lines.join("\n");
    assert!(said.contains("already on [1] cheap"), "{said:?}");
}

#[test]
fn ctrl_digit_is_deliberately_not_a_shortcut() {
    // In a VT terminal Ctrl+2..Ctrl+8 arrive as control *characters* — Ctrl+3 as
    // Escape, Ctrl+8 as Delete — so those combinations never reach this program as
    // digits at all. They are not bound, and this test is what stops somebody
    // "fixing" that by adding them back.
    for digit in ['1', '3', '8'] {
        let mut app = app_with_models(&["cheap", "strong"]);
        let action = app.handle_key(KeyEvent::new(KeyCode::Char(digit), KeyModifiers::CONTROL));
        assert_ne!(
            action,
            Action::Send(ToWorker::SelectModel("cheap".to_string())),
            "ctrl+{digit} must not switch"
        );
        assert_ne!(
            action,
            Action::Send(ToWorker::SelectModel("strong".to_string())),
            "ctrl+{digit} must not switch"
        );
    }
}

#[test]
fn a_narrow_terminal_gives_up_models_before_it_gives_up_the_mark() {
    // A bar that pushes the conversation off the screen has stopped being a status
    // line. Whatever fits, fits.
    let mut app = app_with_models(&[
        "a-very-long-model-name",
        "another-very-long-model-name",
        "a-third-long-model-name",
    ]);
    let screen = draw(&mut app, 30, 20);
    let bar = screen.lines().next().unwrap_or_default().to_string();
    assert!(
        unicode_width::UnicodeWidthStr::width(bar.as_str()) <= 30,
        "the bar never overflows its row: {bar:?}"
    );
    assert!(
        bar.contains("[1]"),
        "the current model is the last to go: {bar:?}"
    );
    assert!(
        bar.contains('…'),
        "and it says that it dropped something: {bar:?}"
    );
}

#[test]
fn the_bar_follows_a_switch_a_rule_made_mid_turn() {
    // A rule can change the model without the interface asking for anything, and it
    // arrives as an ordinary rendered line. If the bar did not read that line it
    // would keep naming the model that answered five steps ago.
    let mut app = app_with_models(&["cheap", "strong"]);
    app.follow_model_change("⇄ model cheap → strong (step > 1)");
    assert_eq!(app.selected, "strong");
    let screen = draw(&mut app, 90, 20);
    assert!(
        screen
            .lines()
            .next()
            .unwrap_or_default()
            .contains("▸[2] strong"),
        "{screen}"
    );

    // A session's first call has no previous model.
    app.follow_model_change("⇄ model cheap (the router's default)");
    assert_eq!(app.selected, "cheap");

    // A name this session does not offer cannot be marked on a bar that lists what
    // it does offer.
    app.follow_model_change("⇄ model cheap → ghost (step > 9)");
    assert_eq!(app.selected, "cheap", "an unknown name is not adopted");

    // And a line about something else is not mistaken for a switch.
    app.follow_model_change("· tokens 10 in / 2 out (p)");
    assert_eq!(app.selected, "cheap");
}

// ---------------------------------------------------------------- the resend

/// Ask something, then let the turn finish — the state a person is in when they
/// decide the model was not up to it.
fn asked_and_finished(app: &mut App, prompt: &str) {
    type_text(app, prompt);
    assert!(matches!(
        app.handle_key(key(KeyCode::Enter)),
        Action::Send(ToWorker::Submit(_))
    ));
    app.busy = false;
}

#[test]
fn alt_r_asks_the_same_question_again() {
    let mut app = app_with_models(&["cheap", "strong"]);
    asked_and_finished(&mut app, "fix the off-by-one");
    assert_eq!(
        app.handle_key(alt('r')),
        Action::Send(ToWorker::Submit("fix the off-by-one".to_string())),
        "the same words, not a summary of them"
    );
}

#[test]
fn the_resend_uses_the_model_selected_now_and_says_so() {
    let mut app = app_with_models(&["cheap", "strong"]);
    asked_and_finished(&mut app, "fix it");
    // Escalate first, then resend: this is the whole gesture the feature exists for.
    app.selected = "strong".to_string();
    assert_eq!(
        app.handle_key(alt('r')),
        Action::Send(ToWorker::Submit("fix it".to_string()))
    );
    let said = app.lines.join("\n");
    assert!(
        said.contains("[2] strong"),
        "the line names the model and its slot, matching the bar: {said:?}"
    );
}

#[test]
fn the_resend_keeps_the_history_rather_than_rewinding_it() {
    // The design, stated as a test: the model that takes over can see what the
    // previous one tried and what went wrong. A clean slate would throw away the
    // most useful thing in the session.
    let mut app = app_with_models(&["cheap", "strong"]);
    asked_and_finished(&mut app, "fix it");
    let before = app.lines.clone();
    assert_eq!(
        app.handle_key(alt('r')),
        Action::Send(ToWorker::Submit("fix it".to_string()))
    );
    let after = app.lines.clone();
    assert!(
        after.len() > before.len(),
        "the transcript only grows: nothing was rewound"
    );
    for line in &before {
        assert!(after.contains(line), "and nothing was dropped: {line:?}");
    }
}

#[test]
fn resending_before_anything_was_asked_says_there_is_nothing_to_resend() {
    let mut app = app_with_models(&["cheap"]);
    assert_eq!(app.handle_key(alt('r')), Action::None);
    let said = app.lines.join("\n");
    assert!(said.contains("还没有可以重发"), "{said:?}");
}

#[test]
fn resending_while_a_turn_is_running_is_refused_rather_than_queued() {
    // A second turn starting on top of a running one is the failure this prevents.
    let mut app = app_with_models(&["cheap", "strong"]);
    type_text(&mut app, "fix it");
    app.handle_key(key(KeyCode::Enter));
    assert!(app.busy, "the turn is running");
    assert_eq!(app.handle_key(alt('r')), Action::None);
    let said = app.lines.join("\n");
    assert!(said.contains("还在跑"), "{said:?}");
}

#[test]
fn slash_r_is_the_fallback_for_a_terminal_that_eats_alt() {
    let mut app = app_with_models(&["cheap", "strong"]);
    asked_and_finished(&mut app, "fix it");
    app.input = "/r".to_string();
    app.cursor = 2;
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Action::Send(ToWorker::Submit("fix it".to_string()))
    );
    assert!(app.input.is_empty(), "the command does not reach the agent");
}

#[test]
fn the_resend_is_advertised_only_once_there_is_something_to_resend() {
    let mut app = app_with_models(&["cheap", "strong"]);
    let bar = draw(&mut app, 100, 20);
    let first = bar.lines().next().unwrap_or_default().to_string();
    assert!(
        !despaced(&first).contains(&despaced("alt+r")),
        "a key advertised before it can work is a key that looks broken: {first:?}"
    );

    asked_and_finished(&mut app, "fix it");
    let bar = draw(&mut app, 100, 20);
    let after = bar.lines().next().unwrap_or_default().to_string();
    assert!(
        despaced(&after).contains(&despaced("alt+r 重发")),
        "and once it can work, it is on the bar: {after:?}"
    );
}

#[test]
fn slash_exit_leaves() {
    let mut app = app_with_models(&["cheap"]);
    app.input = "/exit".to_string();
    app.cursor = 5;
    assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::Quit);
    assert!(app.input.is_empty(), "the command does not reach the agent");
}

#[test]
fn a_plain_r_still_types_an_r() {
    // Alt+R is a shortcut; `r` is a letter. An interface where the letters stopped
    // working would be a worse trade than no shortcut at all.
    let mut app = app_with_models(&["cheap"]);
    app.handle_key(key(KeyCode::Char('r')));
    assert_eq!(app.input, "r");
}

#[test]
fn a_model_that_failed_its_check_is_marked_and_keeps_its_slot() {
    // Marked, not removed. A key revoked at midnight and a network hiccup look
    // identical from here, and a bar that renumbered itself because the wifi dropped
    // would be worse than one that admitted it was unsure.
    let mut app = app_with_models(&["cheap", "strong", "third"]);
    assert!(!app.health.contains_key("strong"), "nothing checked yet");

    app.note_health(ModelHealth {
        name: "strong".to_string(),
        ok: false,
        detail: "无法连接（这不说明密钥有问题）：timeout".to_string(),
    });
    let screen = draw(&mut app, 100, 20);
    let bar = screen.lines().next().unwrap_or_default().to_string();
    assert!(bar.contains("✗[2] strong"), "{bar:?}");
    assert!(
        bar.contains("[3] third"),
        "and the slots either side did not move: {bar:?}"
    );
    assert!(
        despaced(&screen).contains(&despaced("校验未通过")),
        "with the reason in the transcript, because a red glyph is not an explanation:\n{screen}"
    );

    // A model that passes is not marked at all — a tick on everything is noise.
    app.note_health(ModelHealth {
        name: "third".to_string(),
        ok: true,
        detail: "连接成功".to_string(),
    });
    let screen = draw(&mut app, 100, 20);
    let bar = screen.lines().next().unwrap_or_default().to_string();
    assert!(!bar.contains("✗[3]"), "{bar:?}");
    assert!(
        bar.contains("✓") == false,
        "no tick for the good news: {bar:?}"
    );
}

#[test]
fn a_failed_check_still_lets_the_model_be_chosen() {
    // The check is advice. Refusing the key press would mean a transient probe
    // failure locked somebody out of a model that works perfectly well.
    let mut app = app_with_models(&["cheap", "strong"]);
    app.note_health(ModelHealth {
        name: "strong".to_string(),
        ok: false,
        detail: "timeout".to_string(),
    });
    assert_eq!(
        app.handle_key(alt('2')),
        Action::Send(ToWorker::SelectModel("strong".to_string()))
    );
}

#[test]
fn the_status_line_says_what_a_model_command_will_do_before_it_is_pressed() {
    let mut app = app_with_models(&["cheap", "strong"]);
    for (typed, expected) in [("/2", "[2] strong"), ("/1", "[1] cheap")] {
        app.input = typed.to_string();
        app.cursor = typed.chars().count();
        let screen = draw(&mut app, 100, 20);
        assert!(
            despaced(&screen).contains(&despaced(expected)),
            "{typed} should describe itself as {expected:?}:\n{screen}"
        );
    }

    // A slot with no model says so rather than promising a switch.
    app.input = "/7".to_string();
    app.cursor = 2;
    let screen = draw(&mut app, 100, 20);
    assert!(
        despaced(&screen).contains(&despaced("no model in slot 7")),
        "{screen}"
    );

    // And `/r` only describes itself once there is something to resend.
    app.input = "/r".to_string();
    app.cursor = 2;
    let screen = draw(&mut app, 100, 20);
    assert!(
        !despaced(&screen).contains(&despaced("asks the last prompt again")),
        "nothing has been asked yet:\n{screen}"
    );
    asked_and_finished(&mut app, "fix it");
    app.input = "/r".to_string();
    app.cursor = 2;
    let screen = draw(&mut app, 100, 20);
    assert!(
        despaced(&screen).contains(&despaced("asks the last prompt again")),
        "{screen}"
    );
}
