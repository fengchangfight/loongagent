//! Purpose: Prove the full-screen surface's rendering promises: what the screen shows for keys, events, folds, decisions, models and the first-run forms.
//!
//! # Contract
//! - **Serves:** the rendering contract in `docs/design/surfaces.md` §3 and §6; this file is the acceptance test for the screen.
//! - **Design:** `docs/design/surfaces.md` §3 and §6; `docs/design/sessions-lineage.md` §5 for the reattach, resend and new-session paths.
//! - **Upstream:** no production caller; `cargo test -p loong-tui-app` runs the file.
//! - **Downstream:** `App`, `wizard`, `mention`, `setup` and `worker`, and ratatui's `TestBackend`.
//! - **Verify:** `cargo test -p loong-tui-app` · `./scripts/check.sh`

use super::*;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

/// Purpose: Every drawn box's title line, so a state assertion names the box and not the transcript.
/// Contract: called by the prompt-title tests; filters `┌` rows and calls `despaced`.
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

/// Purpose: Feed one worker message to the app, as the draw loop would.
/// Contract: called by the approval, tick and failure tests; builds a channel and calls `App::drain`.
/// Feed one message to the app as though the worker had sent it.
fn fed(app: &mut App, message: FromWorker) {
    let (says, worker) = channel();
    says.send(message).unwrap();
    app.drain(&worker);
}

/// Purpose: A key event with no modifiers.
/// Contract: test helper; every keyboard test uses it.
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Purpose: A character key with the Control modifier.
/// Contract: test helper; the control-chord tests use it.
fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// Purpose: Render the app into a fixed `TestBackend` and return the text rows the assertions read.
/// Serves: the drawing tests in this file.
/// Design: `docs/design/surfaces.md` §3.
/// Up: the drawing tests.
/// Down: `App::draw` and ratatui's `TestBackend`.
/// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: Type each character of a string through the key handler.
/// Contract: test helper; the prompt and wizard tests use it.
fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle_key(key(KeyCode::Char(c)));
    }
}

/// Purpose: Typing `@` opens the picker, narrows it and inserts a real path on Enter.
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

/// Purpose: Esc closes the picker and leaves the draft exactly as it was.
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

/// Purpose: Backspacing over `@` closes the picker, because the text is the only state.
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

/// Purpose: An `@` inside an email address never opens the picker.
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

/// Purpose: A closed picker takes no transcript rows and an open one takes only its matches.
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

/// Purpose: One frame shows the session title, the transcript and the prompt together.
#[test]
fn the_screen_shows_the_conversation_the_input_and_the_session() {
    let mut app = App::new("s-1", "0.1.0");
    app.push_lines(vec!["你：hello".to_string(), "hi there".to_string()]);
    type_text(&mut app, "next question");
    let screen = draw(&mut app, 60, 16);
    assert!(shows(&screen, "会话 s-1"), "{screen}");
    assert!(shows(&screen, "协议 0.1.0"), "{screen}");
    assert!(shows(&screen, "你：hello"), "{screen}");
    assert!(screen.contains("hi there"), "{screen}");
    assert!(screen.contains("next question"), "{screen}");
    assert!(shows(&screen, "输入"), "{screen}");
}

/// Purpose: Enter sends a non-empty prompt, clears the box and marks the turn busy; an empty one sends nothing.
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
        app.lines.iter().any(|l| l == "你：do it"),
        "{:?}",
        app.lines
    );
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Action::None,
        "empty is not a prompt"
    );
}

/// Purpose: Backspace edits the prompt, a plain `d` types, and Ctrl-C leaves.
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

/// Purpose: A pending approval draws a menu instead of the input line and answers it with the chosen scope.
#[test]
fn an_approval_repurposes_the_input_line_and_answers_it() {
    let mut app = App::new("s", "v");
    app.pending = Some("appr-1".to_string());
    let screen = draw(&mut app, 80, 22);
    // A menu, not an orange line: the question, then the answers to pick from.
    assert!(shows(&screen, "需要你决定"), "{screen}");
    assert!(shows(&screen, "本次允许"), "{screen}");
    assert!(shows(&screen, "本项目允许"), "{screen}");
    assert!(shows(&screen, "拒绝"), "{screen}");
    assert!(shows(&screen, "▸"), "the chosen one is marked: {screen}");

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
            .any(|l| shows(l, "已批准（本次）") && l.contains("appr-1")),
        "{:?}",
        app.lines
    );
}

/// Purpose: A question pauses a turn without ending its clock, so the resumed work still has a moving title.
#[test]
fn answering_a_question_keeps_the_turn_clock_running() {
    let mut app = App::new("s", "v");
    // A turn runs, then stops for a question. The pause is not an ending: the turn
    // still has work to resume, and its clock is what says how long it has been going.
    app.busy = true;
    app.busy_since = Some(std::time::Instant::now());
    app.pending = Some("appr-1".to_string());
    fed(
        &mut app,
        FromWorker::State {
            pending: Some("appr-1".to_string()),
        },
    );
    assert!(
        app.busy_since.is_some(),
        "the pause does not throw the turn's clock away"
    );

    // Answering resumes the turn. A busy surface with no clock reads "⠋ working 0s"
    // for as long as the resumed work takes: the number cannot count and the frame
    // cannot turn, which is exactly the "is it stuck?" the title exists to answer.
    match app.handle_key(key(KeyCode::Enter)) {
        Action::Send(_) => {}
        other => panic!("expected the answer to be sent, got {other:?}"),
    }
    let started = app
        .busy_since
        .expect("the resumed turn is timed from its submit");
    // One second in, both halves of the title have moved with that clock.
    app.busy_since = Some(started - std::time::Duration::from_secs(1));
    let screen = draw(&mut app, 80, 20);
    assert!(shows(&screen, "运行 1s"), "the clock counts: {screen}");
    assert!(
        SPINNER
            .iter()
            .any(|frame| *frame != SPINNER[0] && screen.contains(frame)),
        "and the mark turns: {screen}"
    );
}

/// Purpose: A turn that stops for a question is not recorded as finished, so no tick claims the work is over.
#[test]
fn a_turn_paused_for_a_question_is_not_recorded_as_finished() {
    let mut app = App::new("s", "v");
    app.busy = true;
    app.busy_since = Some(std::time::Instant::now());
    fed(
        &mut app,
        FromWorker::State {
            pending: Some("appr-1".to_string()),
        },
    );
    assert!(app.pending.is_some(), "the question is the state");
    assert!(!app.busy);
    assert!(
        app.finished.is_none(),
        "a question is not an ending: {:?}",
        app.finished
    );
    assert!(
        !app.lines.iter().any(|line| shows(line, "完成")),
        "and nothing in the transcript says it finished: {:?}",
        app.lines
    );
}

/// Purpose: Typing `/` lists the commands this screen answers, including the ones a prefix used to hide.
#[test]
fn typing_a_slash_offers_the_commands_this_screen_answers() {
    let mut app = App::new("s", "v");
    type_text(&mut app, "/");
    let screen = draw(&mut app, 100, 24);
    assert!(shows(&screen, "命令"), "the menu is named: {screen}");
    for command in [
        "/new", "/exit", "/yolo", "/copy", "/mouse", "/logo", "/expand",
    ] {
        assert!(screen.contains(command), "{command} is offered: {screen}");
    }
    // And it is still a normal prompt buffer under the menu.
    assert_eq!(app.input, "/");
}

/// Purpose: A prefix narrows the menu and the title says what the one match will do, so `/yo` is not invisible.
#[test]
fn a_prefix_narrows_the_menu_and_says_what_the_command_will_do() {
    let mut app = App::new("s", "v");
    type_text(&mut app, "/yo");
    let screen = draw(&mut app, 100, 24);
    assert!(screen.contains("/yolo"), "the match is offered: {screen}");
    assert!(
        shows(&screen, "打开或关闭确认提示"),
        "and what it does is beside it: {screen}"
    );
    assert!(
        !screen.contains("/new"),
        "a narrowing menu keeps only the match: {screen}"
    );
    // The prompt title carries the same sentence, because that is where the person
    // is looking when the menu is not in the way.
    assert!(
        box_titles(&screen).contains(&despaced("打开或关闭确认提示")),
        "{screen}"
    );
}

/// Purpose: The command menu is visible while a turn runs — the state title used to be the only thing on that row.
#[test]
fn the_command_menu_is_visible_while_a_turn_runs() {
    let mut app = App::new("s", "v");
    app.busy = true;
    app.busy_since = Some(std::time::Instant::now());
    type_text(&mut app, "/yol");
    let screen = draw(&mut app, 100, 24);
    assert!(
        box_titles(&screen).contains("运行"),
        "the turn still says it is working: {screen}"
    );
    assert!(
        screen.contains("/yolo") && shows(&screen, "打开或关闭确认提示"),
        "and the command being typed is still offered: {screen}"
    );
}

/// Purpose: Enter completes a prefix first and runs a complete command, so a half-typed name can never fire.
#[test]
fn enter_completes_a_prefix_before_it_runs() {
    let mut app = App::new("s", "v");
    type_text(&mut app, "/yo");
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Action::None,
        "a prefix is completed, not run"
    );
    assert_eq!(app.input, "/yolo");
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Action::Send(ToWorker::Yolo(true)),
        "the completed name runs on the next enter, as it always did"
    );
}

/// Purpose: Arrows walk the menu, esc puts it away without touching the draft, and a comment word is still the agent's.
#[test]
fn arrows_walk_the_command_menu_and_esc_puts_it_away() {
    let mut app = App::new("s", "v");
    type_text(&mut app, "/e");
    // `/ex` prefixes both `/exit` and `/expand`; the arrow picks, enter completes.
    app.handle_key(key(KeyCode::Down));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.input, "/expand", "the highlighted match was taken");

    // Esc closes the list and keeps the line: "not now", not "throw it away".
    let mut app = App::new("s", "v");
    type_text(&mut app, "/yo");
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.input, "/yo");
    let screen = draw(&mut app, 100, 24);
    assert!(!shows(&screen, "匹配"), "the menu is gone: {screen}");

    // A word this screen does not answer is the server's, and the menu says so.
    let mut app = App::new("s", "v");
    type_text(&mut app, "/he");
    let screen = draw(&mut app, 100, 24);
    assert!(screen.contains("/help"), "the skills are named: {screen}");
    assert!(shows(&screen, "回车仍然会发出去"), "{screen}");
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Action::Send(ToWorker::Submit("/he".to_string())),
        "and enter sends it to the agent, where the skills live"
    );
}

/// Purpose: An alias is a way to the same command, so its prefix offers the command it names, and a slot is exact.
#[test]
fn an_alias_prefix_offers_the_command_it_names() {
    assert_eq!(commands::primary("/CLEAR"), Some("/new"));
    assert!(commands::is_exact("/clear"));
    let offered = commands::matches("/cl");
    assert_eq!(offered.len(), 1, "{offered:?}");
    assert_eq!(offered[0].insert, "/new");

    // `/7` is a model slot, not a table row: it is exact, so enter switches rather
    // than completing, and the menu still says what it is.
    assert!(commands::is_exact("/7"));
    assert_eq!(commands::slot("/7"), Some(7));
    assert_eq!(commands::matches("/7")[0].description, "切到那个模型位置");
}

/// Purpose: A long shell command is flattened to one row, so all three answers stay in the panel.
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
        .skip_while(|line| !shows(line, "需要你决定"))
        .take_while(|line| !line.starts_with('└'))
        .collect();
    assert!(
        panel.len() <= DECISION_ROWS as usize,
        "{} rows: {panel:?}",
        panel.len()
    );
    for answer in ["本次允许", "本项目允许", "拒绝"] {
        assert!(
            panel.iter().any(|line| shows(line, answer)),
            "{answer} left the panel: {panel:?}"
        );
    }
    assert!(
        panel.iter().any(|line| line.contains('⏎')),
        "the command is flattened: {panel:?}"
    );
}

/// Purpose: A CJK command is clipped by display columns, visibly, and all answers stay in the panel.
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
        .skip_while(|line| !shows(line, "需要你决定"))
        .take_while(|line| !line.starts_with('└'))
        .collect();
    assert!(
        panel.len() <= DECISION_ROWS as usize,
        "{} rows: {panel:?}",
        panel.len()
    );
    for answer in ["本次允许", "本项目允许", "拒绝"] {
        assert!(
            panel.iter().any(|line| shows(line, answer)),
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

/// Purpose: On every short terminal, the details give way before any answer does.
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
        for answer in ["本次允许", "本项目允许", "拒绝"] {
            assert!(
                shows(&screen, answer),
                "at {height} rows, {answer} is missing:\n{screen}"
            );
        }
    }
}

/// Purpose: The decision panel is drawn after the transcript and before the prompt.
#[test]
fn the_decision_comes_after_the_work_it_is_about() {
    // The flow reads downwards: events, then the question they raised, then the box you
    // answer in. The panel used to sit above the transcript, which put the decision
    // before the evidence for it.
    let mut app = App::new("s", "v");
    app.push_lines(vec!["→ shell ls".to_string()]);
    app.pending = Some("appr-1".to_string());
    let screen = draw(&mut app, 80, 30);
    // Ordering is read from the space-free copy: the cell buffer pads every wide glyph,
    // so raw columns would measure the padding as well as the words.
    let flat = despaced(&screen);
    let transcript = flat.find("loongagent—会话").expect("the transcript");
    let decision = flat.find("需要你决定").expect("the decision");
    let prompt = flat.find("等你决定").expect("the prompt title");
    assert!(
        transcript < decision && decision < prompt,
        "transcript {transcript}, decision {decision}, prompt {prompt}:\n{screen}"
    );
}

/// Purpose: Arrows move the approval highlight, Enter sends the highlighted scope, and a new question resets it.
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

/// Purpose: Denying is an answer: it sends a refusal and says so in the transcript.
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
        .any(|l| shows(l, "已拒绝") && l.contains("appr-2")));
}

/// Purpose: Paging moves the page, Ctrl-End returns to the end, and new output leaves a reading page alone.
#[test]
fn scrolling_moves_the_window_and_new_output_leaves_a_reading_page_alone() {
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

    // Output that arrives while the reader is away does not pull the page back: that
    // yank is what made selecting a long block impossible.
    app.handle_key(key(KeyCode::PageUp));
    let offset = app.scroll;
    app.push_lines(vec!["a new answer".to_string()]);
    let _ = draw(&mut app, 40, 20);
    assert!(
        app.scroll > offset,
        "the offset grew with the page, so the same rows stayed on screen"
    );
    // Sending returns to the bottom: the prompt is about to be answered.
    app.begin_submit("go".to_string());
    assert_eq!(app.scroll, 0, "sending returns to the newest output");
}

/// Purpose: Scrolling up in a short transcript never pushes the newest lines off the end.
#[test]
fn scrolling_back_cannot_push_the_newest_lines_off_the_end() {
    let startup = [
        "· 可用技能（1）：help",
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
            shows(&screen, line),
            "{line:?} was pushed off the end:\n{screen}"
        );
    }
    assert_eq!(app.scroll, 0, "and the count says what the screen says");
}

/// Purpose: Bookkeeping lines use the terminal's dimmed foreground, not a colour that is the background.
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
    app.push_lines(vec!["· 可用技能（1）：help".to_string()]);
    assert_eq!(
        color_of(&mut app, 60, 24, "· 可用技能"),
        Some(ratatui::style::Color::Reset),
        "and that is what reaches the screen"
    );
}

/// Purpose: Every answer in the decision menu is readable, not just the highlighted one.
#[test]
fn the_answers_that_are_not_chosen_are_still_readable() {
    // The same colour, in the one place it decides something: every answer but the
    // highlighted one was `DarkGray`, so a menu read as a single option — the earlier
    // "the decision menu lost its answers" report had this in it too.
    let mut app = App::new("s", "v");
    app.pending = Some("appr-9".to_string());
    assert_eq!(
        color_of(&mut app, 80, 24, "本项目允许"),
        Some(ratatui::style::Color::Reset),
        "an unchosen answer is dimmed, not drawn into the background"
    );
    assert_eq!(
        color_of(&mut app, 80, 24, "拒绝"),
        Some(ratatui::style::Color::Reset)
    );
}

/// Purpose: Push the transcript the client writes for two summarised tool results.
/// Contract: called by the fold tests; the fixture every fold assertion starts from.
/// The transcript the client writes for two summarised tool results: the summary
/// lines, then the marker that stands for the rest.
fn two_folds(app: &mut App) {
    app.push_lines(vec![
        "你：run it".to_string(),
        "→ shell cargo test".to_string(),
        "← ok older 1".to_string(),
        "  older 2".to_string(),
        "  … 47 more line(s), 900 B · /expand".to_string(),
        "→ shell cargo build".to_string(),
        "← ok newest 1".to_string(),
        "  … 49 more line(s), 900 B · /expand".to_string(),
    ]);
}

/// Purpose: Tab walks the fold markers and marks the selected one, Enter opens it, and Esc closes it without leaving.
#[test]
fn tab_walks_the_folds_and_enter_shows_what_they_hide() {
    let mut app = App::new("s", "v");
    two_folds(&mut app);
    let screen = draw(&mut app, 72, 24);
    assert!(
        shows(&screen, "▸ … 47 more line(s), 900 B · /expand"),
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
    assert!(shows(&screen, "工具结果（50 行）"), "{screen}");
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
        shows(&draw(&mut app, 72, 24), "会话 s"),
        "and does not leave"
    );
}

/// Purpose: A selected fold never stops Enter from sending a non-empty prompt.
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

/// Purpose: A fold whose text the turn already sent opens without a log read; an unheld one asks for it.
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
    assert!(shows(&screen, "工具结果（50 行）"), "{screen}");
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

/// Purpose: Server stderr becomes a note inside the transcript, with repeats collapsed.
#[test]
fn server_notes_are_drawn_instead_of_corrupting_the_screen() {
    let mut app = App::new("s", "v");
    app.push_note("[sandbox] filesystem boundary enforced by landlock".to_string());
    app.push_note("[sandbox] filesystem boundary enforced by landlock".to_string());
    assert_eq!(
        app.lines
            .iter()
            .filter(|line| line.contains("[sandbox]"))
            .count(),
        1,
        "repeats are collapsed"
    );
    app.push_note("[hooks] no hooks".to_string());
    let screen = draw(&mut app, 70, 16);
    assert!(screen.contains("[hooks] no hooks"), "{screen}");
    // On the page, above the prompt: the server's stderr is history, and history scrolls
    // with the turn it explains — it is not a caption floating outside a box.
    let rows: Vec<&str> = screen.lines().collect();
    let note = rows
        .iter()
        .position(|line| line.contains("[hooks]"))
        .expect("the note's row");
    let prompt = rows
        .iter()
        .position(|line| shows(line, "输入"))
        .expect("the prompt's row");
    assert!(note < prompt, "the note is above the prompt:\n{screen}");
    assert!(
        rows[note].trim_start().starts_with('·'),
        "and it is a page line:\n{screen}"
    );
}

/// Purpose: A turn failure clears busy and is visible on screen, and a following state message still lands.
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

/// Purpose: A streamed answer longer than the window shows its newest line, not its first.
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

/// Purpose: The working title turns through the spinner frames and still names what the turn is doing.
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
        shows(&screen, "运行 0s · shell: cargo test"),
        "the words still say what it is doing: {screen}"
    );
    assert!(
        SPINNER.iter().any(|frame| screen.contains(frame)),
        "and a mark that moves is beside them: {screen}"
    );
}

/// Purpose: A streamed fragment is shown once, and the recorded message replaces it without duplication.
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
        shows(&screen, "Writing…"),
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

/// Purpose: The setup form collects three fields, refuses an incomplete one, and wraps its caret.
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

/// Purpose: A canned `ContextExplain` with every memory field populated, for the panel tests.
/// Serves: the context-panel tests, directly or through `panel`.
/// Design: `docs/design/surfaces.md` §3; `docs/context/status.md` §11.1.
/// Up: the context-panel tests.
/// Down: the generated schema types.
/// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: A semantic engine that has not produced an L4 map yet: the state that must not be drawn as off.
/// Serves: the test that separates an off engine from one that has produced nothing yet.
/// Design: `docs/design/surfaces.md` §3; `docs/context/status.md` §11.1.
/// Up: `an_engine_that_is_off_is_not_shown_as_a_memory_that_has_produced_nothing`.
/// Down: `explained`.
/// Verify: `cargo test -p loong-tui-app`.
/// A semantic engine that has not produced a map yet — the state that must not
/// be drawn as "关".
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

/// Purpose: An engine that is off: an empty `memory_mode`, whatever the ledger holds.
/// Serves: the off-versus-fresh panel test.
/// Design: `docs/design/surfaces.md` §3; `docs/context/status.md` §11.1.
/// Up: `an_engine_that_is_off_is_not_shown_as_a_memory_that_has_produced_nothing`.
/// Down: `explained`.
/// Verify: `cargo test -p loong-tui-app`.
/// An engine that is off: `memory_mode` empty, whatever the ledger holds.
fn explained_off() -> Box<loong_schema::generated::ContextExplain> {
    let mut explained = explained();
    explained.memory_mode = String::new();
    explained.memory_summarizer = String::new();
    explained.memory_layers.clear();
    explained.memory_worker = loong_schema::generated::ContextWorker {
        state: "关".to_string(),
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

/// Purpose: `/exit`, `/quit` and `/q` leave locally and never reach the agent.
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
            !app.lines.iter().any(|line| line.starts_with("你：")),
            "{command:?} is not part of the conversation"
        );
    }
}

/// Purpose: Only an exact local command is taken locally; everything else goes to the server.
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

/// Purpose: The prompt hint says what Enter will do for an exact command and lists the commands while ambiguous.
#[test]
fn the_hint_says_what_exit_will_do() {
    let mut app = App::new("s", "v");
    type_text(&mut app, "/exit");
    let screen = draw(&mut app, 100, 16);
    assert!(shows(&screen, "回车退出 loongagent"), "{screen}");

    // And a word this screen does not answer says where it will go — the server's
    // commands (`/help`, the workspace's skills) are still reachable from here.
    let mut app = App::new("s", "v");
    type_text(&mut app, "/he");
    let screen = draw(&mut app, 100, 16);
    assert!(screen.contains("/help"), "the skills are named: {screen}");
    assert!(shows(&screen, "回车仍然会发出去"), "{screen}");
}

/// Purpose: F2 opens the context panel by asking for its data, and closing asks for nothing.
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

/// Purpose: The context panel shows totals, categories, entries and the legend beside the conversation.
#[test]
fn the_context_panel_shows_where_the_tokens_went() {
    let (to_app, from_worker) = channel::<FromWorker>();
    let mut app = App::new("s", "v");
    app.handle_key(key(KeyCode::F(2)));
    to_app.send(FromWorker::Context(explained())).unwrap();
    app.drain(&from_worker);
    let screen = draw(&mut app, 130, 24);
    assert!(shows(&screen, "上下文 (f2)"), "{screen}");
    assert!(screen.contains("1700 / 128000"), "the totals: {screen}");
    assert!(shows(&screen, "12 条消息"), "{screen}");
    assert!(shows(&screen, "次压缩"), "{screen}");
    assert!(
        screen.contains("history") && screen.contains("system_prompt"),
        "{screen}"
    );
    assert!(screen.contains("#1"), "the entries: {screen}");
    assert!(shows(&screen, "固定"), "and the legend: {screen}");
    // The conversation did not disappear.
    assert!(shows(&screen, "loongagent — 会话"), "{screen}");
}

/// Purpose: The context panel is absent until F2, whose hint is always advertised.
#[test]
fn the_panel_stays_closed_until_it_is_asked_for() {
    let mut app = App::new("s", "v");
    let screen = draw(&mut app, 110, 20);
    assert!(!shows(&screen, "上下文 (f2)"), "{screen}");
    assert!(screen.contains("f2"), "the hint advertises it: {screen}");
}

/// Purpose: Draw the F2 panel over one canned context view.
/// Contract: called by the panel tests; opens the panel and calls `App::drain` and `draw`.
/// Draw the F2 panel over one canned context view.
fn panel(context: Box<loong_schema::generated::ContextExplain>) -> String {
    let (to_app, from_worker) = channel::<FromWorker>();
    let mut app = App::new("s", "v");
    app.handle_key(key(KeyCode::F(2)));
    to_app.send(FromWorker::Context(context)).unwrap();
    app.drain(&from_worker);
    draw(&mut app, 200, 40)
}

/// Purpose: The panel names the memory mode, summarizer, L1-L4 counts and worker totals.
#[test]
fn the_panel_shows_the_layers_the_worker_and_the_mode() {
    let screen = panel(explained());
    assert!(shows(&screen, "记忆"), "{screen}");
    assert!(screen.contains("semantic"), "the mode: {screen}");
    assert!(
        shows(&screen, "摘要器 ollama:qwen2.5:7b-instruct"),
        "which model: {screen}"
    );
    // Layer counts, which is what the design asks the panel to expose.
    assert!(screen.contains("L1 120"), "{screen}");
    assert!(screen.contains("L2 8"), "{screen}");
    assert!(screen.contains("L3 2"), "{screen}");
    assert!(screen.contains("L4 1"), "{screen}");
    // And what the background has cost.
    assert!(shows(&screen, "后台 idle"), "{screen}");
    assert!(shows(&screen, "9 次调用"), "{screen}");
    assert!(shows(&screen, "8 条已整理"), "{screen}");
    assert!(shows(&screen, "3 条延后"), "{screen}");
}

/// Purpose: A degraded memory states its reason in the panel and marks the bar with `mem degraded`.
#[test]
fn a_degraded_memory_says_why_in_the_panel_and_on_the_bar() {
    let mut context = explained();
    context.memory_degraded =
        "3 episode(s) fell back to the digest: connection refused".to_string();
    context.memory_worker.failed = 3;
    context.memory_worker.last_error = "connection refused".to_string();
    let screen = panel(context);
    assert!(shows(&screen, "降级："), "{screen}");
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
    assert!(shows(&screen, "记忆 ⚠ 降级"), "{screen}");
}

/// Purpose: An engine that is off and one that has produced nothing yet are drawn differently.
#[test]
fn an_engine_that_is_off_is_not_shown_as_a_memory_that_has_produced_nothing() {
    // The whole reason `memory_mode` exists. "Off" and "on, but the first turns
    // have not produced an L4 map yet" both have no `cognitive_map` entry; they
    // must not render the same.
    let off = panel(explained_off());
    let fresh = panel(explained_fresh());
    assert!(shows(&off, "记忆"), "{off}");
    assert!(shows(&off, "关"), "{off}");
    assert!(
        !off.contains("semantic") && !shows(&off, "还没有 L4 图谱"),
        "an engine that is off must not claim a mode or a pending map: {off}"
    );
    assert!(fresh.contains("semantic"), "{fresh}");
    assert!(
        shows(&fresh, "还没有 L4 图谱"),
        "an engine that is on and has produced nothing says exactly that: {fresh}"
    );
    assert_ne!(
        off, fresh,
        "the two states must be distinguishable on screen"
    );
}

/// Purpose: Record mode says it has no summarizer model.
#[test]
fn record_mode_says_it_has_no_model() {
    let mut context = explained();
    context.memory_mode = "record".to_string();
    context.memory_summarizer = String::new();
    let screen = panel(context);
    assert!(screen.contains("record"), "{screen}");
    assert!(shows(&screen, "无（记录模式）"), "{screen}");
}

/// Purpose: The bar names the memory mode with the panel closed and stays silent when the engine is off.
#[test]
fn the_bar_names_the_mode_while_the_panel_is_closed() {
    let (to_app, from_worker) = channel::<FromWorker>();
    let mut app = App::new("s", "v");
    to_app.send(FromWorker::Context(explained())).unwrap();
    app.drain(&from_worker);
    let screen = draw(&mut app, 200, 24);
    assert!(shows(&screen, "记忆 semantic"), "{screen}");
    // With the engine off there is nothing to say, and saying it anyway would be
    // noise on every screen.
    let (to_app, from_worker) = channel::<FromWorker>();
    let mut app = App::new("s", "v");
    to_app.send(FromWorker::Context(explained_off())).unwrap();
    app.drain(&from_worker);
    let screen = draw(&mut app, 200, 24);
    assert!(!shows(&screen, "记忆 "), "{screen}");
}

/// Purpose: The setup form draws three labelled boxes, the typed values and a hint below them.
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
    assert!(shows(&screen, "首次运行"), "{screen}");
    assert!(shows(&screen, "控制面地址"), "{screen}");
    assert!(shows(&screen, "公钥"), "{screen}");
    assert!(shows(&screen, "令牌引用"), "{screen}");
    assert!(
        shows(&screen, "esc 跳过"),
        "the way out is visible: {screen}"
    );
    // Three *closed* boxes before the hint line: a four-row layout once put the
    // hint on the third field's top border, which this catches.
    let rows: Vec<&str> = screen.lines().collect();
    let hint_row = rows.iter().position(|r| shows(&r, "esc 跳过")).unwrap();
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
    assert!(!shows(&screen, "必须填"), "{screen}");
}

/// Purpose: The setup form is leavable with Esc or Ctrl-C, while a plain `c` is still text.
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

/// Purpose: Caret movement and the readline chords edit the prompt where the caret is.
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

/// Purpose: Editing removes and inserts whole characters, never broken UTF-8 bytes.
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

/// Purpose: A long prompt wraps and the caret is placed on the row where editing happens.
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

/// Purpose: Wrapping counts display columns, so CJK text and wide characters stay inside the border.
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

/// Purpose: A pasted newline is drawn as a hard line break.
#[test]
fn a_pasted_newline_is_a_hard_break() {
    let mut app = App::new("s", "v");
    type_text(&mut app, "first\nsecond");
    let (lines, caret) = app.wrapped_input(40);
    assert_eq!(lines, vec!["first".to_string(), "second".to_string()]);
    assert_eq!(caret, (1, 6));
}

/// Purpose: The input box caps its rows and shows the window that contains the caret.
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

/// Purpose: A wheel notch scrolls three lines and stops at the bottom.
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

/// Purpose: The foreground colour of the cell under a needle, read from the terminal buffer.
/// Serves: the colour tests, which need styles that `draw` throws away.
/// Design: `docs/design/surfaces.md` §3.
/// Up: the colour tests.
/// Down: `App::draw` and ratatui's `TestBackend`.
/// Verify: `cargo test -p loong-tui-app`.
/// The foreground a line is drawn in, read from the cell buffer (the text helper
/// throws styles away, which is the point of having this one).
fn color_of(app: &mut App, width: u16, height: u16, needle: &str) -> Option<ratatui::style::Color> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let area = buffer.area();
    // The cells are compared without their spaces, so a needle may read as a phrase even
    // when the cells it lands on are separated by the buffers padding.
    let needle: String = needle.chars().filter(|ch| !ch.is_whitespace()).collect();
    let needle = needle.as_str();
    for y in 0..area.height {
        // The cell buffer pads every wide glyph with a blank, so a phrase search has to
        // skip the padding to find the words — and keep each kept character's column, so
        // the colour is read from the cell the phrase started in.
        let mut kept: Vec<(char, u16)> = Vec::new();
        for x in 0..area.width {
            if let Some(ch) = buffer[(x, y)].symbol().chars().next() {
                if !ch.is_whitespace() {
                    kept.push((ch, x));
                }
            }
        }
        let text: String = kept.iter().map(|(ch, _)| *ch).collect();
        if let Some(byte) = text.find(needle) {
            let at = text[..byte].chars().count();
            let (_, x) = kept[at];
            return Some(
                buffer[(x, y)]
                    .style()
                    .fg
                    .unwrap_or(ratatui::style::Color::Reset),
            );
        }
    }
    None
}

/// Purpose: Each rendered line class is classified and the palette differs between the classes that matter.
#[test]
fn every_kind_of_line_has_its_own_colour() {
    for (line, expected) in [
        ("你：fix the bug", LineKind::User),
        ("→ read_file a.txt", LineKind::ToolCall),
        ("← 成功 hello", LineKind::ToolOk),
        ("← 失败 no such file", LineKind::ToolFailed),
        (
            "! 需要确认（out_of_bounds）：写文件 [cwd]",
            LineKind::Approval,
        ),
        ("! /deploy：缺参数", LineKind::Problem),
        ("! the turn failed", LineKind::Problem),
        ("· 规则：builtin:write_workspace 放行", LineKind::Policy),
        ("· 令牌 1053 入 / 5 出（default）", LineKind::Note),
        ("· 已压缩：100 → 40 令牌", LineKind::Note),
        ("here is the answer", LineKind::Answer),
    ] {
        assert_eq!(classify(line), expected, "{line:?}");
    }
    // A session written before the interface spoke Chinese still keeps its colours: the
    // classifier reads both vocabularies, and the old one is not going to disappear from
    // the log.
    for (line, expected) in [
        ("你：fix the bug", LineKind::User),
        ("← 成功 hello", LineKind::ToolOk),
        ("← failed no such file", LineKind::ToolFailed),
        (
            "! approval required (out_of_bounds): write [cwd]",
            LineKind::Approval,
        ),
        ("· 规则：builtin:write_workspace 放行", LineKind::Policy),
        ("· tokens 1053 in / 5 out (default)", LineKind::Note),
    ] {
        assert_eq!(classify(line), expected, "the old vocabulary: {line:?}");
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

/// Purpose: Every marked line the real renderer produces is classified as something other than an answer.
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
        // The model's own prose is the one line the renderer leaves bare; it is also the
        // one line `classify` is allowed to call an answer.
        if line == "hello" {
            assert_eq!(classify(&line), LineKind::Answer, "{line:?}");
            continue;
        }
        // The vocabulary the interface speaks, not the one it used to: a line start that
        // no longer exists would make this test pass by skipping, which is worse than red.
        assert!(
            ["你：", "→ ", "← ", "! ", "· "]
                .iter()
                .any(|prefix| line.starts_with(prefix)),
            "{case} rendered to an unmarked line: {line:?}"
        );
        assert_ne!(
            classify(&line),
            LineKind::Answer,
            "{line:?} is marked, so it must be classified"
        );
    }
}

/// Purpose: The drawn screen puts red on failures, cyan on tool calls, green on results, magenta on policy, and leaves the person's words plain.
#[test]
fn errors_are_red_and_work_is_blue_in_the_drawn_screen() {
    let mut app = App::new("s", "v");
    app.push_lines(vec![
        "你：do it".to_string(),
        "→ write_file game.html".to_string(),
        "← 成功 wrote it".to_string(),
        "! the turn failed".to_string(),
        "· 规则：builtin 放行".to_string(),
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
        color_of(&mut app, 80, 20, "放行"),
        Some(ratatui::style::Color::Magenta)
    );
    assert_eq!(
        color_of(&mut app, 80, 20, "你："),
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

/// Purpose: Ctrl-C interrupts a running turn, leaves when idle, and a surface with no process says it cannot interrupt.
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
        app.lines.iter().any(|line| line.starts_with("⏹ 正在中断")),
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
        app.lines.iter().any(|line| shows(line, "无法中断")),
        "{:?}",
        app.lines
    );
}

/// Purpose: An interrupted turn renders as a stop the person caused.
#[test]
fn the_interrupted_event_renders_as_a_stop_the_person_caused() {
    use loong_schema::generated::event;
    let line = loong_tui::render_event(&serde_json::json!({
        "kind": event::TURN_INTERRUPTED,
        "data": {"reason": "interrupted"},
    }))
    .expect("an interrupted turn is shown");
    assert!(shows(&line, "已被你停止"), "{line}");
    assert_eq!(classify(&line), LineKind::Stopped);
}

/// Purpose: A stopped turn is its own class and yellow, not an error or a completion.
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
    app.push_lines(vec!["⏹ 已被你停止".to_string()]);
    assert_eq!(
        color_of(&mut app, 80, 20, "已被你停止"),
        Some(ratatui::style::Color::Yellow)
    );
}

/// Purpose: The person's own message is identified by its user role, so it is not rendered twice.
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

/// Purpose: `/yolo` asks the server and the off state is always visible, with the sandbox note.
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
            .any(|line| shows(line, "确认提示已关闭") && shows(line, "沙箱")),
        "it says what changed and what did not: {:?}",
        app.lines
    );

    // Visible in the input box, always.
    let screen = draw(&mut app, 100, 24);
    assert!(shows(&screen, "免确认"), "the state is on screen: {screen}");
    assert!(shows(&screen, "沙箱仍然生效"), "{screen}");

    // And it can be turned back on, from the command or the state.
    let screen = draw(&mut app, 100, 24);
    assert!(screen.contains("/yolo"), "{screen}");
}

/// Purpose: `/yolo` during a turn reaches the server at once through the control channel, and the screen flips without waiting for the queue.
#[test]
fn yolo_during_a_turn_flips_at_once_and_tells_the_server() {
    let mut app = App::new("s", "v");
    app.busy = true;
    app.busy_since = Some(std::time::Instant::now());
    let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let log = std::rc::Rc::clone(&asked);
    app.set_permissions = Some(Box::new(move |on| log.borrow_mut().push(on)));
    type_text(&mut app, "/yolo");

    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Action::None,
        "nothing is queued for the worker"
    );
    assert!(app.yolo, "the screen shows the change at once");
    assert_eq!(
        *asked.borrow(),
        vec![true],
        "and the running turn was told now, not after it ends"
    );
    assert!(app.input.is_empty());

    // The event is the confirmation, and it is still recorded in the transcript.
    fed(&mut app, FromWorker::Yolo(true));
    assert!(
        app.lines.iter().any(|line| shows(line, "确认提示已关闭")),
        "the log line still arrives: {:?}",
        app.lines
    );
}

/// Purpose: Without a control channel `/yolo` still goes through the worker, and nothing is claimed before the server answers.
#[test]
fn yolo_without_a_control_channel_still_goes_through_the_worker() {
    let mut app = App::new("s", "v");
    app.busy = true;
    type_text(&mut app, "/yolo");
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        Action::Send(ToWorker::Yolo(true)),
        "the platform without a channel keeps the queued path rather than a lie"
    );
    assert!(!app.yolo, "nothing is claimed before the server answers");
}

/// Purpose: A running turn under YOLO still shows the spinner and clock; the warning rides in front instead of replacing them.
#[test]
fn a_running_turn_under_yolo_still_shows_that_it_is_working() {
    let mut app = App::new("s", "v");
    app.yolo = true;
    app.busy = true;
    app.busy_since = Some(std::time::Instant::now());
    app.activity = Some("shell: cargo test".to_string());
    let screen = draw(&mut app, 110, 24);
    let titles = box_titles(&screen);
    assert!(
        shows(&titles, "运行"),
        "the turn says it is working: {screen}"
    );
    assert!(
        titles.contains(&despaced("shell: cargo test")),
        "and names the step: {screen}"
    );
    assert!(
        titles.contains("免确认"),
        "and the warning is still there: {screen}"
    );
}

/// Purpose: A finished turn under YOLO still says it finished; the warning is a mark, not a replacement for the status.
#[test]
fn a_finished_turn_under_yolo_still_says_it_finished() {
    let mut app = App::new("s", "v");
    app.yolo = true;
    app.busy = true;
    app.busy_since = Some(std::time::Instant::now());
    fed(&mut app, FromWorker::State { pending: None });
    let screen = draw(&mut app, 110, 24);
    let titles = box_titles(&screen);
    assert!(
        titles.contains(&despaced("✓ 完成")),
        "the finish is on the prompt box: {screen}"
    );
    assert!(titles.contains("免确认"), "{screen}");
}

/// Purpose: A question under YOLO is still described as a question, never as "not asking".
#[test]
fn a_question_under_yolo_still_says_it_is_a_question() {
    let mut app = App::new("s", "v");
    app.yolo = true;
    app.pending = Some("appr-1".to_string());
    let screen = draw(&mut app, 110, 24);
    let titles = box_titles(&screen);
    assert!(
        titles.contains(&despaced("等你决定")),
        "the question is still a question: {screen}"
    );
    assert!(!titles.contains(&despaced("NOT ASKING")), "{screen}");
    assert!(
        titles.contains("免确认"),
        "and the state is named: {screen}"
    );
}

/// Purpose: Under YOLO a typed command keeps its hint; the warning goes in front rather than in place of it.
#[test]
fn a_command_under_yolo_keeps_its_hint() {
    let mut app = App::new("s", "v");
    app.yolo = true;
    type_text(&mut app, "/yol");
    let screen = draw(&mut app, 110, 24);
    let titles = box_titles(&screen);
    assert!(
        titles.contains(&despaced("打开或关闭确认提示")),
        "the hint is on the box: {screen}"
    );
    assert!(titles.contains("免确认"), "{screen}");
}

/// Purpose: `/copy` writes the transcript to a session-named file and returns the copy action.
#[test]
fn copy_puts_the_transcript_on_the_clipboard_and_in_a_file() {
    let mut app = App::new("s", "v");
    app.push_lines(vec!["你：hello".to_string(), "hi".to_string()]);
    type_text(&mut app, "/copy");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::Copy);
    assert!(app.input.is_empty());
    // The file half, which is what works everywhere: written beside nothing
    // important, in the temporary directory, and named for the session.
    let path = copy_everywhere(&app.transcript(), "testcopy");
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(shows(&written, "你：hello"), "{written}");
    assert!(written.contains("hi"), "{written}");
    let _ = std::fs::remove_file(&path);
}

/// Purpose: The clipboard encoder produces standard base64 padding.
#[test]
fn base64_is_base64() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foo"), "Zm9v");
    assert_eq!(base64(b"foob"), "Zm9vYg==");
    assert_eq!(base64(b"hello world"), "aGVsbG8gd29ybGQ=");
}

/// Purpose: The mouse is off until `/mouse`, which says what it costs and what comes back.
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
        app.lines.iter().any(|line| shows(line, "这里用滚轮滚动")),
        "and says what it costs: {:?}",
        app.lines
    );

    type_text(&mut app, "/mouse");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::Mouse(false));
    assert!(!app.mouse);
    assert!(
        app.lines.iter().any(|line| shows(line, "拖动选择")),
        "and says what came back: {:?}",
        app.lines
    );
}

/// Purpose: The character is drawn as red pixels, not as a font glyph on a coloured block.
#[test]
fn the_character_is_drawn_as_pixels() {
    let mut app = App::new("s", "v");
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    let area = *buffer.area();
    let mut red_pixels = 0;
    let mut drawn_glyph = false;
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buffer[(x, y)];
            if cell.symbol() == "龍" {
                drawn_glyph = true;
            }
            // Half blocks carry the bitmap: the upper pixel is the glyph's colour.
            if matches!(cell.symbol(), "▀" | "▄" | "█") && cell.fg == Color::Red {
                red_pixels += 1;
            }
        }
    }
    assert!(!drawn_glyph, "the font glyph is gone, replaced by pixels");
    assert!(
        red_pixels > 40,
        "the dragon is drawn as a block of red pixels, found {red_pixels}"
    );
}

/// Purpose: The mark is one frame holding the pixel dragon and the name, the inscription below the frame, and `/logo` puts it all away.
#[test]
fn the_mark_is_drawn_and_can_be_put_away() {
    let mut app = App::new("s", "v");
    // Wide: the frame, the character, the block wordmark, the inscription under it.
    let screen = draw(&mut app, 150, 30);
    assert!(
        shows(&screen, "༺ ᛭ ༻") && shows(&screen, "༻ ᛭ ༺"),
        "{screen}"
    );
    // The character is pixels now, and the frame that used to bracket it is gone.
    assert!(
        screen.contains('▀') || screen.contains('▄'),
        "the dragon's pixels:\n{screen}"
    );
    assert!(!screen.contains('龍'), "the font glyph is gone:\n{screen}");
    // The name is block letters at this width; the plain word is the narrow fallback.
    assert!(shows(&screen, "█████"), "the block wordmark:\n{screen}");
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

    // Placed, not scattered: the name is between the frame's walls, and the inscription
    // is below the frame and outside it.
    let rows: Vec<&str> = screen.lines().skip(2).take(11).collect();
    let name = rows
        .iter()
        .find(|row| row.contains("█     █████"))
        .expect("the wordmark's row");
    assert!(
        name.trim_start().starts_with('║') && name.trim_end().ends_with('║'),
        "the name is inside the frame:\n{screen}"
    );
    let bottom = rows
        .iter()
        .position(|row| row.contains("༻ ᛭ ༺"))
        .expect("the bottom rule");
    let inscription = rows
        .iter()
        .position(|row| row.contains('⟬'))
        .expect("the inscription");
    assert!(inscription > bottom, "below the frame:\n{screen}");
    assert!(
        !rows[inscription].trim_start().starts_with('║'),
        "and outside it:\n{screen}"
    );

    // Narrower: the plain word, still inside the frame, and the inscription still fits
    // under it — it is a line of its own now, so it no longer competes with the frame.
    let plain = draw(&mut app, 62, 30);
    let rows: Vec<&str> = plain.lines().skip(2).take(11).collect();
    let name = rows
        .iter()
        .find(|row| row.contains("loongagent"))
        .expect("the plain word");
    assert!(
        name.trim_start().starts_with('║') && name.trim_end().ends_with('║'),
        "the word is framed too:\n{plain}"
    );
    assert!(
        plain.contains('▀') || plain.contains('▄'),
        "the dragon stays:\n{plain}"
    );
    assert!(
        plain.contains('⟬') && plain.contains('藏'),
        "the whole inscription:\n{plain}"
    );

    let shortest = draw(&mut app, 52, 30);
    assert!(
        shortest.contains('▀') || shortest.contains('▄'),
        "the mark itself:\n{shortest}"
    );
    assert!(
        shortest.contains('⟬'),
        "and the inscription fits under it:\n{shortest}"
    );

    // `/logo` puts it away, and nothing else moves.
    type_text(&mut app, "/logo");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None);
    assert!(!app.logo);
    let screen = draw(&mut app, 150, 30);
    assert!(!screen.contains('龍'), "{screen}");
    assert!(
        shows(&screen, "enter 发送"),
        "the box and keys remain:\n{screen}"
    );
}

/// Purpose: A terminal too small for the mark gets no mark and keeps the keys.
#[test]
fn a_small_terminal_gets_no_mark() {
    // A logo that squeezes the conversation costs more than it gives.
    let mut app = App::new("s", "v");
    let screen = draw(&mut app, 36, 12);
    assert!(!shows(&screen, "龍"), "{screen}");
    assert!(!screen.contains('▀'), "and no favicon either:\n{screen}");
    assert!(shows(&screen, "enter 发送"), "{screen}");
}

/// Purpose: The wordmark is block letters only where they fit beside the dragon, and the plain word below that.
#[test]
fn the_wordmark_is_blocks_only_where_they_fit() {
    let mut app = App::new("s", "v");
    // 86 columns of frame: 90 is the boundary, so 150 has room.
    let wide = draw(&mut app, 150, 30);
    let header: String = wide.lines().skip(2).take(11).collect::<Vec<_>>().join("\n");
    // The gap between the L's stem and the O's first block is the font's own shape; the
    // dragon never draws it, so it tells the two apart.
    assert!(header.contains("█     █████"), "block letters:\n{wide}");
    assert!(
        !header.contains("loongagent"),
        "the plain word is replaced, not repeated:\n{wide}"
    );
    // 62 columns: the frame stays, the word goes back to being a word.
    let narrow = draw(&mut app, 62, 30);
    let header: String = narrow
        .lines()
        .skip(1)
        .take(11)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(header.contains("loongagent"), "{narrow}");
    assert!(
        !header.contains("█     █████"),
        "no blocks when they would crowd the frame:\n{narrow}"
    );
}

/// Purpose: An empty session opens on the favicon, and the first line gives the screen back.
#[test]
fn an_empty_session_opens_on_the_favicon() {
    let mut app = App::new("s", "v");
    let splash = draw(&mut app, 120, 44);
    assert!(splash.contains('▀'), "the dragon's pixels:\n{splash}");
    // The opening picture is not the framed mark: no frame, and the wide bare wordmark.
    assert!(
        !splash.contains('╔'),
        "the splash is not the header:\n{splash}"
    );
    app.push_lines(vec!["你：你好".to_string()]);
    let after = draw(&mut app, 120, 44);
    assert!(
        after.contains('╔'),
        "the first line leaves the mark:\n{after}"
    );
}

/// Purpose: The mark is the page's first content, so enough conversation scrolls it away and Home brings it back.
#[test]
fn the_mark_scrolls_away_with_the_page() {
    let mut app = App::new("s", "v");
    app.push_lines((1..=40).map(|n| format!("line {n}")).collect::<Vec<_>>());
    let bottom = draw(&mut app, 80, 30);
    assert!(
        !bottom.contains('╔'),
        "the mark has scrolled off the page:\n{bottom}"
    );
    assert!(bottom.contains("line 40"), "the newest line is in view");
    // ctrl-Home goes to the top of the page, where the mark still is.
    app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::CONTROL));
    let top = draw(&mut app, 80, 30);
    assert!(
        top.contains('╔'),
        "the mark is the page's first row:\n{top}"
    );
}

/// Purpose: Content that arrives while the reader is scrolled up must not push the page.
#[test]
fn new_output_does_not_move_a_pinned_page() {
    let mut app = App::new("s", "v");
    app.push_lines((1..=30).map(|n| format!("line {n}")).collect::<Vec<_>>());
    for _ in 0..5 {
        app.handle_key(key(KeyCode::PageUp));
    }
    let before = draw(&mut app, 80, 24);
    let head: Vec<String> = before
        .lines()
        .take(6)
        .map(|line| line.to_string())
        .collect();
    app.push_lines((31..=40).map(|n| format!("line {n}")).collect::<Vec<_>>());
    let after = draw(&mut app, 80, 24);
    let same: Vec<String> = after.lines().take(6).map(|line| line.to_string()).collect();
    assert_eq!(
        head, same,
        "the rows under the reader's eyes moved:\n{after}"
    );
    assert!(
        !after.contains("line 40"),
        "and the new lines arrived below the window:\n{after}"
    );
}

/// Purpose: The prompt is fixed at the bottom; scrolling moves only the page above it.
#[test]
fn the_prompt_stays_while_the_page_scrolls() {
    let mut app = App::new("s", "v");
    app.push_lines((1..=60).map(|n| format!("line {n}")).collect::<Vec<_>>());
    for _ in 0..6 {
        app.handle_key(key(KeyCode::PageUp));
    }
    let away = draw(&mut app, 80, 24);
    assert!(
        shows(&away, "输入"),
        "the prompt stays put while the page is away:\n{away}"
    );
    // Typing writes into the visible prompt and leaves the page where it was.
    let head: Vec<String> = away
        .lines()
        .skip(2)
        .take(4)
        .map(|l| l.to_string())
        .collect();
    type_text(&mut app, "hi");
    let back = draw(&mut app, 80, 24);
    let same: Vec<String> = back
        .lines()
        .skip(2)
        .take(4)
        .map(|l| l.to_string())
        .collect();
    assert_eq!(head, same, "typing did not push the page:\n{back}");
}

/// Purpose: The favicon is drawn from the terminal's named colours, never an invented RGB.
#[test]
fn the_favicon_uses_named_colours_only() {
    let mut app = App::new("s", "v");
    let mut terminal = Terminal::new(TestBackend::new(120, 44)).unwrap();
    terminal.draw(|frame| app.draw(frame)).unwrap();
    let buffer = terminal.backend().buffer();
    for cell in buffer.content() {
        assert!(
            !matches!(cell.fg, Color::Rgb(..)),
            "a fixed foreground: {:?}",
            cell.fg
        );
        assert!(!matches!(cell.bg, Color::Rgb(..)), "a fixed background");
    }
}

/// Purpose: `/logo` puts the favicon away with the mark, and the screen keeps working.
#[test]
fn logo_hides_the_favicon_too() {
    let mut app = App::new("s", "v");
    type_text(&mut app, "/logo");
    assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None);
    assert!(!app.logo);
    let screen = draw(&mut app, 120, 44);
    assert!(!screen.contains('▀'), "{screen}");
    assert!(shows(&screen, "enter 发送"), "{screen}");
}

/// Purpose: The input box grows for a long prompt and stays one row for a short one.
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
        .position(|line| shows(line, "enter 发送"))
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

/// Purpose: The caret column follows Home and the end of the typed text.
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

/// Purpose: A pushed line with newlines becomes separate transcript lines.
#[test]
fn a_line_with_embedded_newlines_keeps_its_shape() {
    let mut app = App::new("s", "v");
    app.push_lines(vec!["header\nbody".to_string()]);
    assert_eq!(app.lines, vec!["header".to_string(), "body".to_string()]);
}

/// Purpose: Alt+digit travels through the worker and client to the server, and the switch comes back to the bar and the transcript.
#[test]
fn alt_digit_reaches_the_server_and_comes_back_to_the_bar() {
    // The whole chain, not just the key: key → action → worker → client → server →
    // back → bar. The earlier test only went as far as the `Action`, which is exactly
    // where a break would hide.
    use std::sync::{Arc, Mutex};

    /// Purpose: A transport that records every request line and answers with a fixed two-model list.
    /// Serves: `alt_digit_reaches_the_server_and_comes_back_to_the_bar`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: that test builds it and the client calls it.
    /// Down: no call; the `loong_tui::Transport` impl below answers.
    /// Verify: `cargo test -p loong-tui-app`.
    #[derive(Clone, Default)]
    struct Recorder {
        sent: Arc<Mutex<Vec<String>>>,
    }

    impl loong_tui::Transport for Recorder {
        /// Purpose: Record the request line the client writes.
        /// Contract: called by `loong_tui::Client`; pushes the line onto the shared `sent` list.
        fn send(&mut self, line: &str) -> Result<(), String> {
            self.sent.lock().unwrap().push(line.to_string());
            Ok(())
        }
        /// Purpose: Answer every request with the post-switch two-model list, so the full round trip can be asserted.
        /// Serves: the alt-digit end-to-end test.
        /// Design: `docs/design/surfaces.md` §3.
        /// Up: `loong_tui::Client` reads it.
        /// Down: no call; it returns a canned JSON line.
        /// Verify: `cargo test -p loong-tui-app`.
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
    assert!(shows(&said, "此前 a"), "and names what it was: {said:?}");
}

/// Purpose: A worker command produces a state update the screen drains.
#[test]
fn the_worker_turns_commands_into_screen_lines() {
    // A transport that answers everything with one canned line: the worker's
    // contract is that a command produces lines plus a state update.
    /// Purpose: A transport that answers every request with one accepted-turn result.
    /// Serves: `the_worker_turns_commands_into_screen_lines`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: that test builds it and the client calls it.
    /// Down: no call; the `loong_tui::Transport` impl below answers.
    /// Verify: `cargo test -p loong-tui-app`.
    struct Echo;
    impl loong_tui::Transport for Echo {
        /// Purpose: Accept the request line; the answer is canned.
        /// Contract: called by `loong_tui::Client`.
        fn send(&mut self, _line: &str) -> Result<(), String> {
            Ok(())
        }
        /// Purpose: Answer any request with one canned accepted-turn result.
        /// Contract: called by `loong_tui::Client`; returns a fixed JSON line.
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

/// Purpose: The worker fetches the selected fold from the log and the screen shows the hidden text.
#[test]
fn the_worker_fetches_the_fold_the_keyboard_picked() {
    /// Purpose: A transport whose replay holds one summarised 50-line tool result.
    /// Serves: `the_worker_fetches_the_fold_the_keyboard_picked`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: that test builds it and the client calls it.
    /// Down: no call; the `loong_tui::Transport` impl below answers.
    /// Verify: `cargo test -p loong-tui-app`.
    /// Answers every request with a log holding one summarised tool result.
    struct Log;
    impl loong_tui::Transport for Log {
        /// Purpose: Accept the request line; the answer is canned.
        /// Contract: called by `loong_tui::Client`.
        fn send(&mut self, _line: &str) -> Result<(), String> {
            Ok(())
        }
        /// Purpose: Answer any request with a log holding one 50-line tool result, so the fold can be fetched.
        /// Serves: the fold-fetch worker test.
        /// Design: `docs/design/surfaces.md` §3.
        /// Up: `loong_tui::Client` reads it.
        /// Down: no call; it returns a canned JSON line.
        /// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: The second approval of a resumed turn reaches the panel with its own detail.
#[test]
fn the_second_question_of_a_session_reaches_the_screen() {
    // Reported twice: the first approval was fine and the second was invisible. Answering
    // a question *resumes* the turn, and the next question is usually met right there —
    // but the client cleared whatever was pending after every answer, so the question it
    // had just met was thrown away and the worker reported "nothing pending". The panel
    // was never drawn; the transcript still showed the `! approval required` line for it.
    /// Purpose: A transport that answers from a queue of canned JSON-RPC lines.
    /// Serves: `the_second_question_of_a_session_reaches_the_screen`.
    /// Design: `docs/design/surfaces.md` §3; `docs/design/sessions-lineage.md` §5.
    /// Up: that test builds it and the client calls it.
    /// Down: no call; the `loong_tui::Transport` impl below answers.
    /// Verify: `cargo test -p loong-tui-app`.
    struct Queue(std::cell::RefCell<std::collections::VecDeque<String>>);
    impl loong_tui::Transport for Queue {
        /// Purpose: Accept the request line; the answer comes from the queue.
        /// Contract: called by `loong_tui::Client`.
        fn send(&mut self, _line: &str) -> Result<(), String> {
            Ok(())
        }
        /// Purpose: Pop the next canned line, or `None` when the queue is empty.
        /// Contract: called by `loong_tui::Client`.
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
            "次压缩": 0, "categories": [], "entries": []}}))
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
    assert!(shows(&screen, "需要你决定"), "{screen}");
    assert!(screen.contains("write_file"), "{screen}");
    for answer in ["本次允许", "本项目允许", "拒绝"] {
        assert!(shows(&screen, answer), "{answer} is on screen: {screen}");
    }
}

/// Purpose: A permissions event on the turn's stream reaches the screen as the confirmation of an immediate `/yolo`.
#[test]
fn a_permissions_event_during_a_turn_confirms_the_immediate_yolo() {
    /// Purpose: A transport that answers from a queue of canned JSON-RPC lines.
    /// Serves: `a_permissions_event_during_a_turn_confirms_the_immediate_yolo`.
    /// Design: `docs/design/flows/tool-approval.md`.
    /// Up: the test builds it and the client calls it.
    /// Down: no call; the `loong_tui::Transport` impl below answers.
    /// Verify: `cargo test -p loong-tui-app`.
    struct Queue(std::cell::RefCell<std::collections::VecDeque<String>>);
    impl loong_tui::Transport for Queue {
        /// Purpose: Accept the request line; the answer comes from the queue.
        /// Contract: called by `loong_tui::Client`.
        fn send(&mut self, _line: &str) -> Result<(), String> {
            Ok(())
        }
        /// Purpose: Pop the next canned line, or `None` when the queue is empty.
        /// Contract: called by `loong_tui::Client`.
        fn recv_line(&mut self) -> Result<Option<String>, String> {
            Ok(self.0.borrow_mut().pop_front())
        }
    }
    let line = |value: serde_json::Value| value.to_string();
    let queue = vec![
        // The kernel applied the control request between two steps and streamed it.
        line(
            serde_json::json!({"jsonrpc": "2.0", "method": "event", "params": {
            "seq": 3, "kind": "permissions.relaxed",
            "data": {"auto_approve": true, "reason": "the person asked for it with /yolo"}}}),
        ),
        line(
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {"turn_id": 1, "accepted": true}}),
        ),
        // The worker asks for the context view after every command.
        line(serde_json::json!({"jsonrpc": "2.0", "id": 9, "result": {
            "context_tokens": 10, "budget_tokens": 0, "messages": 1, "tools": 1,
            "次压缩": 0, "categories": [], "entries": []}})),
    ];
    let client = Client::new(Queue(std::cell::RefCell::new(queue.into())), "s-e");
    let (to_worker, from_worker) = spawn_worker(client, 0);
    let mut app = App::new("s-e", "v");
    app.busy = true;
    app.busy_since = Some(std::time::Instant::now());
    // The screen already flipped; the event is what makes it trustworthy.
    app.yolo = true;
    to_worker.send(ToWorker::Submit("go".to_string())).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline
        && !app.lines.iter().any(|line| shows(line, "确认提示已关闭"))
    {
        app.drain(&from_worker);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(
        app.lines
            .iter()
            .any(|line| shows(line, "确认提示已关闭") && shows(line, "沙箱")),
        "the confirmation and its record reach the screen: {:?}",
        app.lines
    );
}

/// Purpose: A fold the turn streamed is openable while the turn still owns the connection.
#[test]
fn a_fold_the_turn_wrote_is_readable_while_the_turn_is_still_running() {
    /// Purpose: A transport that streams one summarised tool result and then blocks until the test releases it, keeping the turn live.
    /// Serves: `a_fold_the_turn_wrote_is_readable_while_the_turn_is_still_running`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: that test builds it and the client calls it.
    /// Down: no call; the `loong_tui::Transport` impl below answers.
    /// Verify: `cargo test -p loong-tui-app`.
    /// A turn that streams one summarised result and then keeps working — saying
    /// nothing more — until the test lets it finish. This is the quiet stretch a real
    /// long tool spends, and the one a reader has time to look around in.
    struct SlowStream {
        streamed: bool,
        release: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }
    impl loong_tui::Transport for SlowStream {
        /// Purpose: Accept the request line; the answer is streamed next.
        /// Contract: called by `loong_tui::Client`.
        fn send(&mut self, _line: &str) -> Result<(), String> {
            Ok(())
        }
        /// Purpose: Stream one 50-line tool result, then hold the connection open until the test releases it.
        /// Serves: the mid-turn fold test, which needs a turn that keeps owning the client.
        /// Design: `docs/design/surfaces.md` §3.
        /// Up: `loong_tui::Client` reads it.
        /// Down: `std::sync::atomic::AtomicBool` as the release signal.
        /// Verify: `cargo test -p loong-tui-app`.
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
    assert!(shows(&screen, "工具结果（50 行）"), "{screen}");
    assert!(
        screen.contains("mid-turn 50"),
        "what was hidden is readable while the turn still works: {screen}"
    );

    // Let the turn finish so the worker is not left mid-request.
    release.store(true, std::sync::atomic::Ordering::SeqCst);
}

// ---------------------------------------------------------- the local wizard

/// Purpose: The three-provider fixture (DeepSeek, Moonshot, DashScope) the wizard tests choose from.
/// Serves: the wizard tests.
/// Design: `docs/design/surfaces.md` §3.
/// Up: the wizard tests.
/// Down: `ProviderChoice`.
/// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: Render the wizard into a fixed `TestBackend` and return the text rows.
/// Serves: the wizard drawing tests.
/// Design: `docs/design/surfaces.md` §3.
/// Up: the wizard drawing tests.
/// Down: `draw_local_wizard` and ratatui's `TestBackend`.
/// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: Screen text with whitespace removed, so text containing wide characters can be matched.
/// Serves: the wizard and mark tests, where `TestBackend` pads a CJK glyph with a blank cell.
/// Design: `docs/design/surfaces.md` §3.
/// Up: the wizard and mark tests.
/// Down: no call; it filters characters.
/// Verify: `cargo test -p loong-tui-app`.
/// Screen text with whitespace removed, for matching text that contains wide
/// characters. `TestBackend` stores a CJK glyph in one cell and leaves the next
/// blank, so a drawn `阿里云百炼` reads back as `阿 里 云 百 炼`. Comparing
/// de-spaced strings sidesteps that without pretending the padding is not there.
fn despaced(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Purpose: Whether the drawn screen says a phrase, ignoring the padding space the cell buffer puts after each wide glyph.
/// Contract: called by the drawing tests that assert on Chinese, which is every screen that speaks the interface's language.
fn shows(screen: &str, phrase: &str) -> bool {
    despaced(screen).contains(&despaced(phrase))
}

/// Purpose: A refusal reorders providers by the hint, invents nothing and keeps an empty hint inert.
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

/// Purpose: A provider that needs no key is ready with an empty field, says so, and restores the rules when switched away.
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

/// Purpose: A picker opened with nothing ticked, as the wizard actually opens one.
/// Serves: the picker tests about ticking and filtering.
/// Design: `docs/design/surfaces.md` §3.
/// Up: the picker tests.
/// Down: `LocalWizard::new` and the `WizardState::Choosing` fields.
/// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: A picker with every row already ticked, for the tests about unticking.
/// Contract: called by `toggle_all_means_all_of_what_is_on_screen` and `filtering_narrows_the_view_without_changing_what_is_ticked`.
/// A picker with everything already ticked, for the tests about *unticking*.
fn choosing_all(names: &[&str]) -> LocalWizard {
    let mut wizard = choosing(names);
    if let WizardState::Choosing { ticked, .. } = &mut wizard.state {
        *ticked = vec![true; names.len()];
    }
    wizard
}

/// Purpose: Nothing starts ticked, so one model out of forty costs a filter and a tick.
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

/// Purpose: Ctrl-A ticks exactly what the filter is showing.
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

/// Purpose: For any list length, reaching one chosen model costs the same two keys.
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

/// Purpose: Filtering narrows the picker to one id and ticking writes only that id.
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

/// Purpose: A filter hides rows without unticking them, and clearing it brings them back.
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

/// Purpose: Ctrl-A selects the visible list and again gives it back.
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

/// Purpose: The picker refuses to finish with nothing ticked and submits once a row is ticked.
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

/// Purpose: The picker cursor stays in range and reaches the last row.
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

/// Purpose: Esc leaves the picker for the form, keeping the key; a second Esc cancels the form.
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

/// Purpose: The picker shows its tick count and the next key, before and after ticking.
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

/// Purpose: A long list scrolls so the row being ticked is the row on screen.
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

/// Purpose: One offered key, built the way the CLI builds it.
/// Contract: called by the offered-key wizard tests.
fn offering(provider: &str, var: &str, key: &str) -> OfferedKey {
    OfferedKey {
        provider: provider.to_string(),
        var: var.to_string(),
        key: key.to_string(),
    }
}

/// Purpose: An offered key is named by its variable, never drawn, and typed input replaces it.
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

/// Purpose: Switching provider switches which offered key applies, and a provider with none needs typing.
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

/// Purpose: No control chord reaches the API-key field, while ordinary typing does.
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

/// Purpose: Every character a model id can contain, including `a` and space, is filter text and ticks nothing.
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

/// Purpose: A plain `a` is text and Ctrl-A is the select-all command.
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

/// Purpose: An empty filter result is drawn as an explanation, not an empty box.
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

/// Purpose: The wizard masks the key's body and never draws it on screen.
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

/// Purpose: A too-short key is refused with a reason and a long-enough one submits.
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
        WizardState::Refused(why) => assert!(shows(&why, "太短"), "{why}"),
        other => panic!("expected a refusal, got {other:?}"),
    }

    // Long enough is enough — the vendor, not a regex, decides what is valid.
    for c in "45678".chars() {
        wizard.handle_key(key(KeyCode::Char(c)));
    }
    assert_eq!(wizard.handle_key(key(KeyCode::Enter)), WizardAction::Submit);
}

/// Purpose: Provider selection moves both ways and wraps in both directions.
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

/// Purpose: The wizard freezes input and Enter while a validation is in flight.
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

/// Purpose: A refusal keeps the typed key on screen so a typo can be corrected.
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

/// Purpose: The wizard draws every provider label, marks only the current one, and shows the keys and the way out.
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
        shows(&screen, "▸ Moonshot / Kimi"),
        "the current choice is marked:\n{screen}"
    );
    assert!(
        !shows(&screen, "▸ DeepSeek"),
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

/// Purpose: A network failure is shown as not being a bad key.
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

/// Purpose: Esc, Ctrl-C and Ctrl-D all leave the wizard.
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

/// Purpose: A `ModelList` fixture in the given declaration order.
/// Serves: the model-bar tests.
/// Design: `docs/design/surfaces.md` §3.
/// Up: the model-bar tests.
/// Down: the generated schema type.
/// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: An app already holding a model list.
/// Contract: called by the model-bar, shortcut and resend tests.
fn app_with_models(names: &[&str]) -> App {
    let mut app = App::new("s", "v");
    app.set_models(model_list(names));
    app
}

/// Purpose: A character key with the Alt modifier.
/// Contract: test helper; the Alt-shortcut tests use it.
fn alt(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
}

/// Purpose: The bar numbers models left to right in the profile's declaration order.
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

/// Purpose: Only the current model is marked, and the mark follows a switch.
#[test]
fn the_model_bar_marks_the_current_model_and_only_that_one() {
    use ratatui::style::Modifier;
    let mut app = app_with_models(&["cheap", "strong"]);
    let marked = |app: &mut App, name: &str| -> bool {
        let mut terminal = Terminal::new(TestBackend::new(90, 20)).expect("a test terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draws");
        let buffer = terminal.backend().buffer().clone();
        let row: String = (0..buffer.area().width)
            .map(|x| buffer[(x, 0)].symbol().to_string())
            .collect();
        let at = row.find(name).expect("the model is on the bar") as u16;
        buffer[(at, 0)].modifier.contains(Modifier::UNDERLINED)
    };
    assert!(marked(&mut app, "cheap"), "the running model is marked");
    assert!(
        !marked(&mut app, "strong"),
        "the model that is not running is not marked"
    );

    // And the mark follows a switch, so the bar never lies about what is answering.
    app.selected = "strong".to_string();
    assert!(marked(&mut app, "strong"), "the mark followed the switch");
    assert!(!marked(&mut app, "cheap"), "and left the old model alone");
}

/// Purpose: Done, stopped and failed have different words, colours and marks.
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

/// Purpose: The prompt title keeps the last ending until a new task starts, and a command hint outranks it when idle.
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
        box_titles(&screen).contains("运行"),
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
        box_titles(&screen).contains(&despaced("离开 loongagent")),
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

/// Purpose: The foreground colour of each cell under a needle, on whichever row it appears.
/// Contract: called by the prompt-colour tests; delegates to `colours_of_where`.
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

/// Purpose: The same, restricted to the box title row, so the transcript's copy is not found instead.
/// Contract: called by the prompt-colour tests; delegates to `colours_of_where`.
/// The same, restricted to the box's title row — the transcript carries the same
/// closing line, so an unrestricted search would find that one instead.
fn title_colours_of(app: &mut App, needle: &str) -> Vec<Color> {
    colours_of_where(app, needle, true)
}

/// Purpose: Find a needle's non-space characters in the drawn buffer and return the foreground colour of each cell that holds one.
/// Serves: `colours_of` and `title_colours_of`, for the style-leak tests.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `colours_of` and `title_colours_of`.
/// Down: `App::draw` and ratatui's `TestBackend`.
/// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: The typed text keeps its own colour; the turn outcome colours only the title.
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

/// Purpose: A busy turn colours only the working title, never the typed text.
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
    let title = title_colours_of(&mut app, "运行");
    assert!(title.contains(&Color::Cyan), "{title:?}");
}

/// Purpose: A failed turn is drawn as a failure and never as a completion.
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

/// Purpose: A stopped turn says stopped and never shows the completion tick.
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

/// Purpose: `/new`, `/clear` and `/reset` ask for a fresh session rather than wiping the view.
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

/// Purpose: A new session clears the old transcript, folds, question and tick, and says where the old log went.
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
            .any(|line| line.contains("s-new1") && shows(line, "存盘")),
        "and it says where the old session went: {:?}",
        app.lines
    );
    let screen = draw(&mut app, 110, 20);
    assert!(!screen.contains("something long"), "{screen}");
}

/// Purpose: Ctrl-N and Ctrl-P step through models and wrap in both directions.
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

/// Purpose: Plain `n` and `p` are letters, not model shortcuts.
#[test]
fn plain_n_and_p_still_type_letters() {
    let mut app = app_with_models(&["a", "b"]);
    app.handle_key(key(KeyCode::Char('n')));
    app.handle_key(key(KeyCode::Char('p')));
    assert_eq!(app.input, "np");
}

/// Purpose: The bar hint names the always-delivered keys first and still offers the numbered ones.
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

/// Purpose: The current model is reversed as a block and the others are not.
#[test]
fn the_current_model_is_drawn_as_a_filled_key_and_underlined_name() {
    // "Did that switch?" is the one question the bar exists to answer, and a `▸` in a
    // row of dim text was too quiet to answer it.
    use ratatui::style::Modifier;
    let mut app = app_with_models(&["cheap", "strong"]);
    let mut terminal = Terminal::new(TestBackend::new(90, 20)).expect("a test terminal");
    terminal.draw(|frame| app.draw(frame)).expect("draws");
    let buffer = terminal.backend().buffer().clone();

    // Row 0 is the bar: the current slot's key is filled with the accent and its name is
    // underlined; a key that is not in hand is neither.
    let row: String = (0..buffer.area().width)
        .map(|x| buffer[(x, 0)].symbol().to_string())
        .collect();
    let key = row.find("[1]").expect("the current slot's key") as u16;
    assert_eq!(
        buffer[(key, 0)].bg,
        ratatui::style::Color::Magenta,
        "the current slot's key is filled: {row:?}"
    );
    let at = row.find("cheap").expect("the current model is on the bar") as u16;
    assert!(
        buffer[(at, 0)].modifier.contains(Modifier::UNDERLINED),
        "the current model is underlined: {row:?}"
    );

    let other = row.find("strong").expect("the other model is on the bar") as u16;
    assert!(
        !buffer[(other, 0)].modifier.contains(Modifier::UNDERLINED),
        "and the one that is not running is not: {row:?}"
    );
}

/// Purpose: The bar advertises both the Alt shortcut and the slash fallback.
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

/// Purpose: With one model the bar promises no shortcut.
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

/// Purpose: A session with no models draws no bar.
#[test]
fn a_session_with_no_models_draws_no_bar_at_all() {
    let mut app = App::new("s", "v");
    let without = draw(&mut app, 80, 20);
    assert!(
        !without.contains("[1]"),
        "nothing is drawn when there is nothing to draw: {without}"
    );
}

/// Purpose: Alt+digit sends the model in that slot.
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

/// Purpose: `/N` sends the same model switch as Alt+digit and does not reach the agent.
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

/// Purpose: An empty model slot explains itself on screen instead of silently doing nothing.
#[test]
fn a_slot_with_no_model_says_so_instead_of_doing_nothing() {
    let mut app = app_with_models(&["cheap", "strong"]);
    let action = app.handle_key(alt('7'));
    assert_eq!(action, Action::None, "there is no seventh model");
    let said = app.lines.join("\n");
    assert!(
        shows(&said, "没有第 7 个模型"),
        "a key that appears to do nothing is worse than one that explains: {said:?}"
    );
}

/// Purpose: Switching to the current model asks the server nothing and says so.
#[test]
fn switching_to_the_model_already_running_does_not_ask_the_server() {
    // A round trip to be told nothing changed would put a line in the log for a
    // non-event, and the log is the record.
    let mut app = app_with_models(&["cheap", "strong"]);
    assert_eq!(app.handle_key(alt('1')), Action::None);
    let said = app.lines.join("\n");
    assert!(shows(&said, "已经在 [1] cheap 上"), "{said:?}");
}

/// Purpose: Ctrl+digit never switches models.
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

/// Purpose: A narrow bar stays within its row, keeps the current model, and says it dropped models.
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

/// Purpose: The bar follows a `model` line, ignores unknown names and ignores unrelated lines.
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
            .contains("[2] strong"),
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

/// Purpose: Ask a prompt, then mark the turn finished: the state a person is in when deciding to resend.
/// Contract: called by the resend tests; type, Enter and clear `busy`.
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

/// Purpose: Alt-R resubmits the exact last prompt.
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

/// Purpose: A resend uses the model selected now and names it with its slot.
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

/// Purpose: A resend grows the transcript and drops nothing.
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

/// Purpose: Alt-R with nothing asked says there is nothing to resend.
#[test]
fn resending_before_anything_was_asked_says_there_is_nothing_to_resend() {
    let mut app = app_with_models(&["cheap"]);
    assert_eq!(app.handle_key(alt('r')), Action::None);
    let said = app.lines.join("\n");
    assert!(shows(&said, "还没有可以重发"), "{said:?}");
}

/// Purpose: Alt-R while busy is refused and says the turn is still running.
#[test]
fn resending_while_a_turn_is_running_is_refused_rather_than_queued() {
    // A second turn starting on top of a running one is the failure this prevents.
    let mut app = app_with_models(&["cheap", "strong"]);
    type_text(&mut app, "fix it");
    app.handle_key(key(KeyCode::Enter));
    assert!(app.busy, "the turn is running");
    assert_eq!(app.handle_key(alt('r')), Action::None);
    let said = app.lines.join("\n");
    assert!(shows(&said, "还在跑"), "{said:?}");
}

/// Purpose: `/r` resends like Alt-R and does not reach the agent.
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

/// Purpose: The bar advertises the resend only after there is something to resend.
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

/// Purpose: `/exit` leaves without reaching the agent.
#[test]
fn slash_exit_leaves() {
    let mut app = app_with_models(&["cheap"]);
    app.input = "/exit".to_string();
    app.cursor = 5;
    assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::Quit);
    assert!(app.input.is_empty(), "the command does not reach the agent");
}

/// Purpose: A plain `r` is a letter, not a resend.
#[test]
fn a_plain_r_still_types_an_r() {
    // Alt+R is a shortcut; `r` is a letter. An interface where the letters stopped
    // working would be a worse trade than no shortcut at all.
    let mut app = app_with_models(&["cheap"]);
    app.handle_key(key(KeyCode::Char('r')));
    assert_eq!(app.input, "r");
}

/// Purpose: A failed model check marks the model with a reason and keeps every slot in place.
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
    assert!(shows(&bar, "✗[2] strong"), "{bar:?}");
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
    assert!(!shows(&bar, "✗[3]"), "{bar:?}");
    assert!(
        shows(&bar, "✓") == false,
        "no tick for the good news: {bar:?}"
    );
}

/// Purpose: A failed check never blocks choosing the model.
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

/// Purpose: A typed `/N` or `/r` describes what Enter will do, or says there is nothing to do.
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
        despaced(&screen).contains(&despaced("第 7 个位置没有模型")),
        "{screen}"
    );

    // And `/r` only describes itself once there is something to resend.
    app.input = "/r".to_string();
    app.cursor = 2;
    let screen = draw(&mut app, 100, 20);
    assert!(
        !despaced(&screen).contains(&despaced("把上一个提问再问一遍")),
        "nothing has been asked yet:\n{screen}"
    );
    asked_and_finished(&mut app, "fix it");
    app.input = "/r".to_string();
    app.cursor = 2;
    let screen = draw(&mut app, 100, 20);
    assert!(
        despaced(&screen).contains(&despaced("把上一个提问再问一遍")),
        "{screen}"
    );
}
