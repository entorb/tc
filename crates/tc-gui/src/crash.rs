//! Panic reports to Sentry: tc version, OS and stack trace, no user data.

use std::sync::Arc;

use regex::Regex;
use sentry::protocol::Event;
use sentry::transports::UreqHttpTransportOptions;
use sentry::{ClientInitGuard, ClientOptions, Transport, TransportFactory, TransportOptions};

const DSN: &str = "https://7303d27bb0620b0620821abcdfbd7c43@o4507139041525760.ingest.de.sentry.io/4512502372106320";

/// Sends through the update check's client: corporate proxy and OS certs.
struct Proxied;

impl TransportFactory for Proxied {
    fn create_transport_with_options(&self, options: TransportOptions) -> Arc<dyn Transport> {
        Arc::new(
            UreqHttpTransportOptions::from(options)
                .with_agent(tc_remote::update::agent())
                .build(),
        )
    }
}

/// Starts reporting; the guard flushes pending reports when dropped, so keep it until exit.
pub fn init() -> ClientInitGuard {
    // CI release builds carry a tag: Prod; local builds: Dev
    let (release, env) = match tc_gui::app::VERSION {
        Some(v) => (Some(format!("tc@{v}").into()), "Prod"),
        None => (sentry::release_name!(), "Dev"),
    };
    let home = std::env::home_dir().and_then(|h| home_regex(&h.to_string_lossy()));
    sentry::init((
        DSN,
        ClientOptions::new()
            .maybe_release(release)
            .environment(env)
            .transport(Proxied)
            .before_send(move |e| Some(scrub(e, home.as_ref()))),
    ))
}

/// The home path in any case and with `/`, `\` or Debug-escaped `\\` between its parts.
fn home_regex(home: &str) -> Option<Regex> {
    let parts: Vec<_> = home.split(['/', '\\']).map(regex::escape).collect();
    if parts.iter().all(String::is_empty) {
        return None;
    }
    Regex::new(&format!("(?i){}", parts.join(r"[\\/]+"))).ok()
}

/// Drops the host name and replaces the home path (it holds the user name) in panic texts.
fn scrub(mut e: Event<'static>, home: Option<&Regex>) -> Event<'static> {
    e.server_name = None;
    let Some(home) = home else {
        return e;
    };
    let exceptions = e.exception.values.iter_mut().flat_map(|x| {
        let frames = x.stacktrace.iter_mut().flat_map(|st| st.frames.iter_mut());
        std::iter::once(&mut x.value).chain(frames.map(|f| &mut f.abs_path))
    });
    for text in exceptions.chain([&mut e.message]).flatten() {
        *text = home.replace_all(text, "~").into_owned();
    }
    e
}

#[cfg(test)]
mod tests {
    use sentry::protocol::{Event, Exception};

    use super::home_regex as home;

    #[test]
    fn scrub_drops_host_and_home() {
        let e = Event {
            server_name: Some("pc-of-alice".into()),
            message: Some("open /home/alice/x".into()),
            exception: vec![Exception {
                value: Some("no /home/alice/secret.txt".into()),
                ..Exception::default()
            }]
            .into(),
            ..Event::default()
        };
        let e = super::scrub(e, home("/home/alice").as_ref());
        assert_eq!(e.server_name, None);
        assert_eq!(e.message.as_deref(), Some("open ~/x"));
        assert_eq!(
            e.exception.values[0].value.as_deref(),
            Some("no ~/secret.txt")
        );
    }

    #[test]
    fn scrub_matches_debug_escaped_windows_home() {
        let e = Event {
            message: Some(r"open C:\\Users\\alice\\secret.txt".into()),
            ..Event::default()
        };
        let e = super::scrub(e, home(r"C:\Users\alice").as_ref());
        assert_eq!(e.message.as_deref(), Some(r"open ~\\secret.txt"));
    }

    #[test]
    fn scrub_ignores_case_and_slash_style() {
        let e = Event {
            message: Some(r"a c:\users\ALICE\x b C:/Users/alice/y".into()),
            ..Event::default()
        };
        let e = super::scrub(e, home(r"C:\Users\alice").as_ref());
        assert_eq!(e.message.as_deref(), Some(r"a ~\x b ~/y"));
        assert!(home("/").is_none());
    }

    #[test]
    fn scrub_removes_home_from_frame_paths() {
        use sentry::protocol::{Frame, Stacktrace};

        let e = Event {
            exception: vec![Exception {
                value: Some("clean exception".into()),
                stacktrace: Some(Stacktrace {
                    frames: vec![Frame {
                        abs_path: Some("/home/alice/src/main.rs".into()),
                        function: Some("run".into()),
                        lineno: Some(12),
                        ..Frame::default()
                    }],
                    ..Stacktrace::default()
                }),
                ..Exception::default()
            }]
            .into(),
            ..Event::default()
        };
        let e = super::scrub(e, home("/home/alice").as_ref());
        let exception = &e.exception.values[0];
        assert_eq!(exception.value.as_deref(), Some("clean exception"));
        let frame = &exception.stacktrace.as_ref().unwrap().frames[0];
        assert_eq!(frame.abs_path.as_deref(), Some("~/src/main.rs"));
        assert_eq!(frame.function.as_deref(), Some("run"));
        assert_eq!(frame.lineno, Some(12));
    }
}
