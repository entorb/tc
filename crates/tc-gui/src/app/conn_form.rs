//! Connection form (Insert / F4 in the Ctrl+N list): type dropdown, host, user, port and
//! directory make the URL the list stores; containers can be picked from the running ones.

use super::widgets::step_focus;
use super::{App, Filter, FormRow, Mode, list_nav, list_rows};
use crate::{KeyCode, KeyEvent, KeyModifiers};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use tc_core::cmdline::LineEdit;
use tc_remote::{Pick, Scheme, Url};

/// Type dropdown, in `Scheme::ALL` order.
const TYPES: [&str; 6] = [
    "SFTP (SSH)",
    "FTP",
    "FTPS",
    "Docker",
    "Podman",
    "Kubernetes",
];
const TYPE: usize = 0;
pub const HOST: usize = 1;
const USER: usize = 2;
const PORT: usize = 3;
const DIR: usize = 4;

#[derive(Clone)]
pub struct ConnForm {
    /// Row of the connection list being edited, `None` adds one.
    pub row: Option<usize>,
    pub rows: Vec<FormRow>,
    pub focus: usize,
    pub error: Option<String>,
}

/// Running containers to fill the form from; read in a worker (`rows` `None` until then).
pub struct ConnPick {
    pub form: ConnForm,
    pub rows: Option<Vec<Pick>>,
    pub sel: usize,
    rx: Receiver<tc_core::Result<Vec<Pick>>>,
}

/// Field labels of a type: host, user, port, directory.
fn labels(s: Scheme) -> [&'static str; 4] {
    match s {
        Scheme::Sftp => [
            "Host",
            "User",
            "Port (empty: 22)",
            "Directory (empty: home)",
        ],
        Scheme::Ftp | Scheme::Ftps => [
            "Host",
            "User",
            "Port (empty: 21)",
            "Directory (empty: home)",
        ],
        Scheme::Docker | Scheme::Podman => [
            "Container",
            "User (optional)",
            "Port",
            "Directory (empty: workdir)",
        ],
        Scheme::Kube => [
            "Pod (namespace/pod)",
            "Container (optional)",
            "Port",
            "Directory (empty: workdir)",
        ],
    }
}

impl ConnForm {
    /// Form for a new connection (`row` `None`) or the saved URL `text` of list row `row`.
    pub(super) fn new(row: Option<usize>, text: &str) -> Self {
        let text_row = || FormRow::Text("", LineEdit::default());
        let mut f = Self {
            row,
            rows: vec![
                FormRow::Choice("Type", &TYPES, 0),
                text_row(),
                text_row(),
                text_row(),
                text_row(),
            ],
            focus: HOST,
            error: None,
        };
        match Url::parse(text) {
            Ok(u) => f.fill(&u),
            Err(_) => f.set(HOST, text),
        }
        f.relabel();
        f
    }

    pub fn scheme(&self) -> Scheme {
        match self.rows[TYPE] {
            FormRow::Choice(_, _, i) => Scheme::ALL[i],
            _ => Scheme::Sftp,
        }
    }

    /// Dropdown text of the selected type.
    pub fn type_name(&self) -> &'static str {
        match self.rows[TYPE] {
            FormRow::Choice(_, opts, i) => opts[i],
            _ => "",
        }
    }

    /// Port is no field of a container connection.
    pub fn hidden(&self, row: usize) -> bool {
        row == PORT && self.scheme().is_container()
    }

    fn text(&self, row: usize) -> &str {
        match &self.rows[row] {
            FormRow::Text(_, l) => l.text.trim(),
            _ => "",
        }
    }

    fn set(&mut self, row: usize, text: &str) {
        if let FormRow::Text(_, l) = &mut self.rows[row] {
            *l = LineEdit::from(text.to_owned());
        }
    }

    /// Text fields get the labels of the selected type; their text stays.
    fn relabel(&mut self) {
        for (row, label) in (HOST..=DIR).zip(labels(self.scheme())) {
            if let FormRow::Text(l, _) = &mut self.rows[row] {
                *l = label;
            }
        }
    }

    fn fill(&mut self, u: &Url) {
        if let Some(i) = Scheme::ALL.iter().position(|&s| s == u.scheme) {
            self.rows[TYPE] = FormRow::Choice("Type", &TYPES, i);
        }
        let port = if u.port == u.scheme.default_port() {
            String::new()
        } else {
            u.port.to_string()
        };
        for (row, v) in [
            (HOST, &u.host),
            (USER, &u.user),
            (PORT, &port),
            (DIR, &u.path),
        ] {
            self.set(row, v);
        }
    }

    /// A whole URL typed or pasted into the host field is spread over the fields.
    fn spread_url(&mut self) {
        if self.text(HOST).contains("://")
            && let Ok(u) = Url::parse(self.text(HOST))
        {
            self.fill(&u);
            self.relabel();
        }
    }

    /// Line under the fields: the URL Enter saves and how the type connects.
    pub fn hint(&self) -> String {
        let url = self.url().unwrap_or_else(|_| "-".into());
        let how = match self.scheme() {
            Scheme::Sftp => "Keys or ssh-agent log in, else the password is asked on connect.",
            Scheme::Ftp | Scheme::Ftps => {
                "The password is asked on connect (empty user: anonymous)."
            }
            Scheme::Docker | Scheme::Podman => {
                "Runs the CLI's exec: the container must be running and have sh."
            }
            Scheme::Kube => "Runs kubectl exec (current context): the pod needs sh.",
        };
        format!("URL: {url}\n{how}")
    }

    /// The URL the fields make, or what is wrong with them. A whole URL in the host field
    /// wins over the other fields (spread over them when the focus leaves it).
    pub fn url(&self) -> Result<String, String> {
        let whole = self.text(HOST);
        if whole.contains("://") {
            return Url::parse(whole)
                .map(|_| whole.to_owned())
                .map_err(|e| e.to_string());
        }
        let s = self.scheme();
        let [host_label, ..] = labels(s);
        let mut host = self.text(HOST).to_owned();
        if host.is_empty() {
            return Err(format!("{host_label}: empty"));
        }
        if s == Scheme::Kube && !host.contains('/') {
            host = format!("default/{host}");
        }
        let user = self.text(USER);
        let at = if user.is_empty() { "" } else { "@" };
        let port = match self.text(PORT) {
            p if p.is_empty() || s.is_container() => String::new(),
            p => match p.parse::<u16>() {
                Ok(n) if n > 0 => format!(":{n}"),
                _ => return Err(format!("Port: {p} is no port number")),
            },
        };
        let dir = self.text(DIR);
        let slash = if dir.is_empty() || dir.starts_with('/') {
            ""
        } else {
            "/"
        };
        let url = format!("{}://{user}{at}{host}{port}{slash}{dir}", s.name());
        Url::parse(&url).map(|_| url).map_err(|e| e.to_string())
    }
}

impl App {
    /// Form keys: Tab / Up / Down move, Space / Left / Right change the type, Enter saves,
    /// Esc back to the list.
    pub(super) fn conn_form_key(&mut self, mut f: Box<ConnForm>, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Connections(f.row.unwrap_or(0));
                return;
            }
            KeyCode::F(2) if f.scheme().is_container() => return self.conn_pick_open(*f),
            KeyCode::Enter if f.scheme().is_container() && f.text(HOST).is_empty() => {
                return self.conn_pick_open(*f);
            }
            KeyCode::Enter => match f.url() {
                Ok(url) => return self.conn_save(f.row, &url),
                Err(e) => f.error = Some(e),
            },
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                if f.focus == HOST {
                    f.spread_url();
                }
                let delta = if matches!(key.code, KeyCode::Tab | KeyCode::Down) {
                    1
                } else {
                    -1
                };
                f.focus = step_focus(0..f.rows.len(), f.focus, delta, |i| f.hidden(i));
            }
            _ => {
                if let Some(row) = f.rows.get_mut(f.focus) {
                    row.key(key);
                }
                f.relabel();
                f.error = None;
            }
        }
        self.mode = Mode::ConnEdit(f);
    }

    /// F2 / Enter on an empty container field: list the running containers in a worker.
    fn conn_pick_open(&mut self, form: ConnForm) {
        let (tx, rx) = mpsc::channel();
        let scheme = form.scheme();
        thread::spawn(move || {
            let _ = tx.send(tc_remote::running(scheme));
        });
        self.list_filter = Filter::default();
        self.mode = Mode::ConnPick(Box::new(ConnPick {
            form,
            rows: None,
            sel: 0,
            rx,
        }));
    }

    pub(super) fn conn_pick_tick(&mut self) {
        let Mode::ConnPick(p) = &mut self.mode else {
            return;
        };
        if p.rows.is_some() {
            return;
        }
        let error = match p.rx.try_recv() {
            Err(TryRecvError::Empty) => return,
            Ok(Ok(rows)) if !rows.is_empty() => {
                p.rows = Some(rows);
                return;
            }
            Ok(Ok(_)) => format!("no running {} containers", p.form.type_name()),
            Ok(Err(e)) => e.to_string(),
            Err(TryRecvError::Disconnected) => "container list failed".into(),
        };
        p.form.error = Some(error);
        let form = p.form.clone();
        self.mode = Mode::ConnEdit(Box::new(form));
    }

    /// Picker keys: Up / Down / `/` filter, Enter fills the form, Ctrl+R lists again, Esc back.
    pub(super) fn conn_pick_key(&mut self, mut p: Box<ConnPick>, key: KeyEvent) {
        let Some(rows) = &p.rows else {
            if key.code == KeyCode::Esc {
                self.mode = Mode::ConnEdit(Box::new(p.form));
            } else {
                self.mode = Mode::ConnPick(p);
            }
            return;
        };
        if list_nav(&mut self.list_filter, rows, &mut p.sel, key) {
            self.mode = Mode::ConnPick(p);
            return;
        }
        let shown = list_rows(rows, &self.list_filter).contains(&p.sel);
        let pick = rows.get(p.sel).filter(|_| shown).cloned();
        match (key.code, pick) {
            (KeyCode::Enter, Some(pick)) => {
                let mut form = p.form;
                form.set(HOST, &pick.host);
                form.set(USER, &pick.user);
                form.focus = DIR;
                form.error = None;
                self.mode = Mode::ConnEdit(Box::new(form));
            }
            (KeyCode::Char('r'), _) if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.conn_pick_open(p.form);
            }
            (KeyCode::Esc | KeyCode::F(10) | KeyCode::Char('q'), _) => {
                self.mode = Mode::ConnEdit(Box::new(p.form));
            }
            _ => self.mode = Mode::ConnPick(p),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{key, two_panels};
    use super::*;

    fn form(url: &str) -> ConnForm {
        ConnForm::new(None, url)
    }

    #[test]
    fn fields_make_the_url() {
        let f = form("sftp://me@h:2222/srv");
        assert_eq!(
            (f.text(HOST), f.text(USER), f.text(PORT)),
            ("h", "me", "2222")
        );
        assert_eq!(f.url().unwrap(), "sftp://me@h:2222/srv");
        // default port not shown, not stored
        let f = form("ftp://h:21");
        assert_eq!((f.scheme(), f.text(PORT)), (Scheme::Ftp, ""));
        assert_eq!(f.url().unwrap(), "ftp://h");
        let f = form("kube://app@prod/web/var/log");
        assert_eq!((f.text(HOST), f.text(USER)), ("prod/web", "app"));
        assert_eq!(f.url().unwrap(), "kube://app@prod/web/var/log");
        assert!(f.hidden(PORT));
    }

    #[test]
    fn errors_and_defaults() {
        let mut f = form("");
        assert_eq!(f.scheme(), Scheme::Sftp);
        assert_eq!(f.url().unwrap_err(), "Host: empty");
        f.set(HOST, "h");
        f.set(PORT, "x");
        assert!(f.url().unwrap_err().starts_with("Port:"));
        f.set(PORT, "");
        f.set(DIR, "data");
        assert_eq!(f.url().unwrap(), "sftp://h/data");
        f.rows[TYPE] = FormRow::Choice("Type", &TYPES, 5);
        f.set(PORT, "x"); // hidden for containers: ignored
        assert_eq!(f.url().unwrap(), "kube://default/h/data");
        // a pasted URL that does not parse stays as typed
        let f = form("nonsense");
        assert_eq!(f.text(HOST), "nonsense");
    }

    #[test]
    fn picker_fills_the_form() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = two_panels(dir.path(), dir.path());
        let pick = |host: &str, user: &str| Pick {
            host: host.into(),
            user: user.into(),
            label: host.into(),
        };
        let open = |app: &mut App, rows: Vec<Pick>| {
            let (tx, rx) = mpsc::channel();
            tx.send(Ok(rows)).unwrap();
            let mut form = form("kube://x/y");
            form.set(HOST, "");
            app.mode = Mode::ConnPick(Box::new(ConnPick {
                form,
                rows: None,
                sel: 0,
                rx,
            }));
            app.conn_pick_tick();
        };
        open(
            &mut app,
            vec![pick("prod/api", "app"), pick("prod/api", "sidecar")],
        );
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Enter);
        let Mode::ConnEdit(f) = &app.mode else {
            panic!("back in the form")
        };
        assert_eq!(
            (f.focus, f.url().unwrap()),
            (DIR, "kube://sidecar@prod/api".into())
        );
        // nothing running: back in the form with the reason
        open(&mut app, Vec::new());
        assert!(
            matches!(&app.mode, Mode::ConnEdit(f) if f.error.as_deref() == Some("no running Kubernetes containers"))
        );
    }
}
