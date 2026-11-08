//! Outgoing mail: invitations and share emails (decision 0003-3). On the laptop the server sends
//! to Mailpit (`SMTP_URL`, `smtp://127.0.0.1:1025`); tests use [`MemoryMailer`]. Production
//! sending stays with a person (DESIGN 18.9); no production mail sender is configured.
//!
//! [`SmtpMailer`] is a small SMTP client for a relay that needs no TLS and no login (Mailpit, or
//! a local relay in production). A provider with TLS and login comes later.
//!
//! Mail safety (decision 0003-3, v2 3.3):
//!
//! - Names that go into the subject or a header (the sharer, the item, the space) lose CR, LF
//!   and every other control character (Unicode Cc, U+2028, U+2029), and are cut at 100
//!   characters with an ellipsis ([`header_text`]).
//! - The personal message keeps only LF of the control characters, has at most 1000
//!   characters, and goes out as escaped text in both parts: no Markdown, no HTML, no link is
//!   made from it ([`message_text`]).
//! - Every value in the HTML part is escaped ([`html_escape`]).
//! - Addresses with a control character, a space or one of `<>",;` are refused.

use crate::authz::Role;
use crate::error::{Error, Result};
use crate::tenants::MemberRole;
use async_trait::async_trait;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use time::OffsetDateTime;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// One email.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mail {
    pub to: String,
    pub subject: String,
    /// Plain text.
    pub body: String,
    /// The HTML part. With it the mail goes as `multipart/alternative`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    /// `Reply-To`: the sharer's email, only when it is verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
}

#[async_trait]
pub trait Mailer: Send + Sync {
    async fn send(&self, mail: Mail) -> Result<()>;
}

/// Keeps the mails in memory, for tests.
#[derive(Debug, Default)]
pub struct MemoryMailer {
    sent: Mutex<Vec<Mail>>,
}

impl MemoryMailer {
    /// The mails sent so far.
    pub fn sent(&self) -> Vec<Mail> {
        self.sent.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait]
impl Mailer for MemoryMailer {
    async fn send(&self, mail: Mail) -> Result<()> {
        check_address(&mail.to)?;
        self.sent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(mail);
        Ok(())
    }
}

/// Refuses an address that could break a header or an SMTP command.
pub fn check_address(address: &str) -> Result<()> {
    let bad = address.is_empty()
        || address.len() > 320
        || !address.contains('@')
        || address.chars().any(|c| {
            c.is_control()
                || c.is_whitespace()
                || matches!(c, '<' | '>' | '"' | ',' | ';' | '\u{2028}' | '\u{2029}')
        });
    if bad {
        return Err(Error::Internal(format!(
            "refused to mail an unsafe address ({} characters)",
            address.chars().count()
        )));
    }
    Ok(())
}

/// Sends mail over plain SMTP to a relay without login.
#[derive(Clone, Debug)]
pub struct SmtpMailer {
    host: String,
    port: u16,
    from: String,
}

impl SmtpMailer {
    /// A mailer for `smtp://host:port`, with the sender address `from`.
    pub fn from_url(url: &str, from: &str) -> Result<Self> {
        let u = url::Url::parse(url).map_err(|e| Error::Internal(format!("SMTP_URL: {e}")))?;
        if u.scheme() != "smtp" {
            return Err(Error::Internal("SMTP_URL must start with smtp://".into()));
        }
        Ok(Self {
            host: u.host_str().unwrap_or("127.0.0.1").to_string(),
            port: u.port().unwrap_or(25),
            from: from.to_string(),
        })
    }

    async fn deliver(&self, mail: &Mail) -> std::io::Result<()> {
        let stream = TcpStream::connect((self.host.as_str(), self.port)).await?;
        let (r, mut w) = stream.into_split();
        let mut r = BufReader::new(r);
        expect(&mut r, 220).await?;
        for (cmd, code) in [
            ("EHLO ruum.localhost".to_string(), 250),
            (format!("MAIL FROM:<{}>", self.from), 250),
            (format!("RCPT TO:<{}>", mail.to), 250),
            ("DATA".to_string(), 354),
        ] {
            w.write_all(format!("{cmd}\r\n").as_bytes()).await?;
            expect(&mut r, code).await?;
        }
        let msg = message(&self.from, mail, crate::now());
        w.write_all(msg.as_bytes()).await?;
        expect(&mut r, 250).await?;
        w.write_all(b"QUIT\r\n").await?;
        Ok(())
    }
}

/// The DATA of one mail: headers, the parts, dot stuffing and the final `.` line.
fn message(from: &str, mail: &Mail, now: OffsetDateTime) -> String {
    let date = now
        .format(&time::format_description::well_known::Rfc2822)
        .unwrap_or_default();
    let mut head = format!(
        "From: Ruum <{from}>\r\nTo: <{}>\r\nSubject: {}\r\nDate: {date}\r\nMIME-Version: 1.0\r\n",
        mail.to,
        encode_header(&header_text(&mail.subject, 200)),
    );
    if let Some(r) = mail
        .reply_to
        .as_deref()
        .filter(|r| check_address(r).is_ok())
    {
        head.push_str(&format!("Reply-To: <{r}>\r\n"));
    }
    let mut body = String::new();
    match &mail.html {
        None => {
            head.push_str(
                "Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n",
            );
            body.push_str(&mail.body);
        }
        Some(html) => {
            let boundary = format!("ruum-{}", uuid::Uuid::now_v7().simple());
            head.push_str(&format!(
                "Content-Type: multipart/alternative; boundary=\"{boundary}\"\r\n"
            ));
            for (kind, part) in [("plain", mail.body.as_str()), ("html", html.as_str())] {
                body.push_str(&format!(
                    "--{boundary}\nContent-Type: text/{kind}; charset=utf-8\n\
                     Content-Transfer-Encoding: 8bit\n\n{part}\n"
                ));
            }
            body.push_str(&format!("--{boundary}--\n"));
        }
    }
    let mut msg = head;
    msg.push_str("\r\n");
    for line in body.lines() {
        // Dot stuffing (RFC 5321, section 4.5.2).
        if line.starts_with('.') {
            msg.push('.');
        }
        msg.push_str(line);
        msg.push_str("\r\n");
    }
    msg.push_str(".\r\n");
    msg
}

/// A header value: plain ASCII as it is, anything else as RFC 2047 encoded words (UTF-8,
/// base64), each at most 75 characters long.
fn encode_header(value: &str) -> String {
    if value.is_ascii() {
        return value.to_string();
    }
    let mut words = Vec::new();
    let mut chunk = String::new();
    for c in value.chars() {
        if chunk.len() + c.len_utf8() > 45 {
            words.push(std::mem::take(&mut chunk));
        }
        chunk.push(c);
    }
    if !chunk.is_empty() {
        words.push(chunk);
    }
    words
        .iter()
        .map(|w| {
            format!(
                "=?UTF-8?B?{}?=",
                base64::engine::general_purpose::STANDARD.encode(w.as_bytes())
            )
        })
        .collect::<Vec<_>>()
        .join("\r\n ")
}

/// Reads one (possibly multi-line) SMTP reply and checks its code.
async fn expect<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R, code: u16) -> std::io::Result<()> {
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).await? == 0 {
            return Err(std::io::Error::other(
                "the SMTP server closed the connection",
            ));
        }
        let got: u16 = line.get(..3).and_then(|c| c.parse().ok()).unwrap_or(0);
        if got != code {
            return Err(std::io::Error::other(format!(
                "SMTP: expected {code}, got {}",
                line.trim_end()
            )));
        }
        if line.as_bytes().get(3) != Some(&b'-') {
            return Ok(());
        }
    }
}

#[async_trait]
impl Mailer for SmtpMailer {
    async fn send(&self, mail: Mail) -> Result<()> {
        check_address(&mail.to)?;
        self.deliver(&mail)
            .await
            .map_err(|e| Error::Internal(format!("send mail: {e}")))
    }
}

/// The longest name in a subject or header, in characters.
pub const MAX_HEADER_NAME: usize = 100;
/// The longest personal message, in characters (the contract caps it too).
pub const MAX_MESSAGE: usize = 1000;

fn is_line_break_like(c: char) -> bool {
    matches!(c, '\u{2028}' | '\u{2029}')
}

/// A name for a subject or header: no control characters (Unicode Cc, U+2028, U+2029), at most
/// `max` characters, cut with an ellipsis.
pub fn header_text(s: &str, max: usize) -> String {
    let clean: String = s
        .chars()
        .filter(|c| !c.is_control() && !is_line_break_like(*c))
        .collect();
    let clean = clean.trim();
    if clean.chars().count() <= max {
        return clean.to_string();
    }
    let mut cut: String = clean.chars().take(max.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

/// The personal message: line breaks become LF, other control characters go, at most
/// [`MAX_MESSAGE`] characters.
pub fn message_text(s: &str) -> String {
    s.replace("\r\n", "\n")
        .chars()
        .map(|c| {
            if c == '\r' || is_line_break_like(c) {
                '\n'
            } else {
                c
            }
        })
        .filter(|c| *c == '\n' || !c.is_control())
        .take(MAX_MESSAGE)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Escapes text for the HTML part.
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// A sharing role in words: "can view", "can comment", "can edit", "can manage".
pub fn role_words(role: Role) -> &'static str {
    match role {
        Role::View => "can view",
        Role::Comment => "can comment on",
        Role::Edit => "can edit",
        Role::Manage => "can manage",
    }
}

/// A tenant role in words, for the invitation mail.
pub fn member_role_words(role: MemberRole) -> &'static str {
    match role {
        MemberRole::Guest => "a guest (you see only what people share with you)",
        MemberRole::Member => "a member",
        MemberRole::Admin => "an admin",
        MemberRole::Owner => "an owner",
    }
}

/// A date in words, for example "21 November 2026" (UTC).
pub fn date_words(t: OffsetDateTime) -> String {
    let t = t.to_offset(time::UtcOffset::UTC);
    format!("{} {} {}", t.day(), t.month(), t.year())
}

/// The link to an item in the web UI: `/t/<tenant>/files/<id>` for a folder,
/// `/t/<tenant>/item/<id>` for anything else (decision 0003-3, reviewer 2 #1).
pub fn item_link(public_url: &str, tenant: uuid::Uuid, node: uuid::Uuid, folder: bool) -> String {
    format!(
        "{}/t/{tenant}/{}/{node}",
        public_url.trim_end_matches('/'),
        if folder { "files" } else { "item" }
    )
}

/// What a share email says.
#[derive(Clone, Debug)]
pub struct ShareMail<'a> {
    pub to: &'a str,
    pub sharer: &'a str,
    pub item: &'a str,
    pub folder: bool,
    pub role: Role,
    pub message: Option<&'a str>,
    /// The item link ([`item_link`]). It carries no token.
    pub link: &'a str,
    /// The grant is an email grant: the recipient opens the item by signing in with this
    /// address (rule 2(b)).
    pub email_grant: bool,
    /// The sharer's verified email.
    pub reply_to: Option<&'a str>,
}

/// The share email (J3): `<Sharer> shared "<item>" with you`, the role in words, the optional
/// message, and the item link. No token.
pub fn share_mail(m: &ShareMail<'_>) -> Mail {
    let sharer = header_text(m.sharer, MAX_HEADER_NAME);
    let sharer = if sharer.is_empty() {
        "Someone".to_string()
    } else {
        sharer
    };
    let item = header_text(m.item, MAX_HEADER_NAME);
    let what = if m.folder { "folder" } else { "file" };
    let role = role_words(m.role);
    let message = m.message.map(message_text).filter(|t| !t.is_empty());
    let mut text =
        format!("{sharer} shared the {what} \"{item}\" with you on Ruum. You {role} it.\n\n");
    let mut html = format!(
        "<p>{} shared the {what} &ldquo;{}&rdquo; with you on Ruum. You {role} it.</p>\n",
        html_escape(&sharer),
        html_escape(&item)
    );
    if let Some(msg) = &message {
        text.push_str(&format!("Message from {sharer}:\n{msg}\n\n"));
        html.push_str(&format!(
            "<p>Message from {}:</p>\n<blockquote style=\"white-space: pre-wrap\">{}</blockquote>\n",
            html_escape(&sharer),
            html_escape(msg)
        ));
    }
    text.push_str(&format!("Open it:\n{}\n", m.link));
    html.push_str(&format!(
        "<p><a href=\"{}\">Open the {what}</a></p>\n",
        html_escape(m.link)
    ));
    if m.email_grant {
        let to = header_text(m.to, 320);
        text.push_str(&format!(
            "\nTo open it, sign in to Ruum with this email address ({to}). New to Ruum? \
             Create an account with this address and verify it first.\n"
        ));
        html.push_str(&format!(
            "<p>To open it, sign in to Ruum with this email address ({}). New to Ruum? \
             Create an account with this address and verify it first.</p>\n",
            html_escape(&to)
        ));
    }
    text.push_str("\nIf you didn't expect this email, you can ignore it.\n");
    html.push_str("<p>If you didn&rsquo;t expect this email, you can ignore it.</p>\n");
    Mail {
        to: m.to.to_string(),
        subject: format!("{sharer} shared \"{item}\" with you"),
        body: text,
        html: Some(html),
        reply_to: m.reply_to.map(str::to_string),
    }
}

/// What an invitation email says.
#[derive(Clone, Debug)]
pub struct InvitationMail<'a> {
    pub to: &'a str,
    pub space: &'a str,
    pub inviter: &'a str,
    pub role: MemberRole,
    pub expires_at: OffsetDateTime,
    pub accept_url: &'a str,
    pub message: Option<&'a str>,
}

/// The invitation email (docs/ux/copy.md, section 6): `<Inviter> invited you to <space> on Ruum`,
/// the space, the inviter, the role in words and the end date, with the accept link. (The content role of members comes with decision 0003-1.)
pub fn invitation_mail(m: &InvitationMail<'_>) -> Mail {
    let space = header_text(m.space, MAX_HEADER_NAME);
    let inviter = header_text(m.inviter, MAX_HEADER_NAME);
    let inviter = if inviter.is_empty() {
        "Someone".to_string()
    } else {
        inviter
    };
    let role = member_role_words(m.role);
    let until = date_words(m.expires_at);
    let message = m.message.map(message_text).filter(|t| !t.is_empty());
    let mut text = format!(
        "{inviter} invited you to join the Ruum space \"{space}\" as {role}.\n\n\
         Open this link to join:\n{}\n\n\
         The link works until {until}.\n",
        m.accept_url
    );
    let mut html = format!(
        "<p>{} invited you to join the Ruum space &ldquo;{}&rdquo; as {}.</p>\n\
         <p><a href=\"{}\">Join the space</a></p>\n\
         <p>The link works until {until}.</p>\n",
        html_escape(&inviter),
        html_escape(&space),
        html_escape(role),
        html_escape(m.accept_url)
    );
    if let Some(msg) = &message {
        text.push_str(&format!("\nMessage from {inviter}:\n{msg}\n"));
        html.push_str(&format!(
            "<p>Message from {}:</p>\n<blockquote style=\"white-space: pre-wrap\">{}</blockquote>\n",
            html_escape(&inviter),
            html_escape(msg)
        ));
    }
    text.push_str("\nIf you didn't expect this email, you can ignore it.\n");
    html.push_str("<p>If you didn&rsquo;t expect this email, you can ignore it.</p>\n");
    Mail {
        to: m.to.to_string(),
        subject: format!("{inviter} invited you to {space} on Ruum"),
        body: text,
        html: Some(html),
        reply_to: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invitation(url: &str, message: Option<&str>) -> Mail {
        invitation_mail(&InvitationMail {
            to: "a@b.test",
            space: "Acme",
            inviter: "Mari",
            role: MemberRole::Member,
            expires_at: time::macros::datetime!(2026-11-21 10:00 UTC),
            accept_url: url,
            message,
        })
    }

    #[test]
    fn the_invitation_mail_names_the_space_inviter_role_and_end() {
        let m = invitation("https://x/invitations/accept?token=t", None);
        assert!(m.body.contains("https://x/invitations/accept?token=t"));
        assert_eq!(m.subject, "Mari invited you to Acme on Ruum");
        assert!(m.body.contains("Mari invited you"));
        assert!(m.body.contains("as a member"));
        assert!(m.body.contains("21 November 2026"));
        assert!(m.html.as_deref().unwrap().contains("21 November 2026"));
    }

    fn share(sharer: &str, item: &str, message: Option<&str>) -> Mail {
        share_mail(&ShareMail {
            to: "ben@example.test",
            sharer,
            item,
            folder: true,
            role: Role::Edit,
            message,
            link: "https://ruum.localhost/t/1/files/2",
            email_grant: true,
            reply_to: Some("ann@example.test"),
        })
    }

    #[test]
    fn the_share_mail_has_the_item_link_role_and_no_token() {
        let m = share("Ann", "Plans", Some("Have a look"));
        assert_eq!(m.subject, "Ann shared \"Plans\" with you");
        assert!(m.body.contains("https://ruum.localhost/t/1/files/2"));
        assert!(m.body.contains("You can edit it."));
        assert!(m.body.contains("Message from Ann:\nHave a look"));
        assert!(m.body.contains("ben@example.test"));
        assert!(!m.body.contains("token"));
        assert_eq!(m.reply_to.as_deref(), Some("ann@example.test"));
    }

    #[test]
    fn names_with_line_breaks_give_a_one_line_subject() {
        let m = share("Ann\r\nBcc: evil@x.test", "a\u{2028}b\u{0}", None);
        assert!(!m.subject.contains('\r') && !m.subject.contains('\n'));
        assert!(!m.subject.contains('\u{2028}') && !m.subject.contains('\u{0}'));
        assert_eq!(m.subject, "AnnBcc: evil@x.test shared \"ab\" with you");
        let data = message("no-reply@ruum.localhost", &m, crate::now());
        let headers = data.split("\r\n\r\n").next().unwrap();
        assert!(!headers.lines().any(|l| l.starts_with("Bcc:")), "{headers}");
    }

    #[test]
    fn long_names_are_cut_with_an_ellipsis() {
        let long = "x".repeat(300);
        let m = share(&long, &long, None);
        let first = m.subject.split(" shared").next().unwrap();
        assert_eq!(first.chars().count(), MAX_HEADER_NAME);
        assert!(first.ends_with('…'));
    }

    #[test]
    fn the_html_part_escapes_every_value_and_the_message_makes_no_link() {
        let m = share(
            "<b>Ann</b>",
            "\"x\" & <y>",
            Some("<a href=\"https://evil.test\">click</a> **bold** [x](https://evil.test)"),
        );
        let html = m.html.unwrap();
        assert!(!html.contains("<b>Ann"));
        assert!(html.contains("&lt;b&gt;Ann&lt;/b&gt;"));
        assert!(html.contains("&quot;x&quot; &amp; &lt;y&gt;"));
        assert!(!html.contains("href=\"https://evil.test\""));
        // The only link is the item link.
        assert_eq!(html.matches("<a ").count(), 1);
        assert!(html.contains("href=\"https://ruum.localhost/t/1/files/2\""));
    }

    #[test]
    fn the_message_keeps_line_feeds_only_and_is_capped() {
        assert_eq!(message_text("a\r\nb\u{7}c\u{2029}d"), "a\nbc\nd");
        assert_eq!(message_text(&"y".repeat(2000)).chars().count(), MAX_MESSAGE);
    }

    #[test]
    fn multipart_mails_have_both_parts_and_dot_stuffing() {
        let m = share("Ann", "Plans", Some(".starts with a dot"));
        let data = message("no-reply@ruum.localhost", &m, crate::now());
        assert!(data.contains("Content-Type: multipart/alternative; boundary="));
        assert!(data.contains("Content-Type: text/plain; charset=utf-8"));
        assert!(data.contains("Content-Type: text/html; charset=utf-8"));
        assert!(data.contains("Reply-To: <ann@example.test>\r\n"));
        assert!(data.contains("\r\n..starts with a dot\r\n"));
        assert!(data.ends_with("\r\n.\r\n"));
    }

    #[test]
    fn non_ascii_subjects_are_encoded_words() {
        let m = share("Mart Õun", "Ülevaade", None);
        let data = message("no-reply@ruum.localhost", &m, crate::now());
        let subject = data.lines().find(|l| l.starts_with("Subject: ")).unwrap();
        assert!(subject.starts_with("Subject: =?UTF-8?B?"), "{subject}");
        assert!(data.is_ascii() || data.contains("Mart Õun shared"));
    }

    #[test]
    fn unsafe_addresses_are_refused() {
        for bad in [
            "a b@x.test",
            "a@x.test>\r\nRCPT",
            "<a@x.test>",
            "",
            "nobody",
        ] {
            assert!(check_address(bad).is_err(), "{bad:?}");
        }
        assert!(check_address("Mari.Maasikas+ruum@example.test").is_ok());
    }

    /// Sends one mail to the relay of `SMTP_URL` (Mailpit on the laptop). Run with
    /// `cargo test -p ruum-core -- --ignored smtp`.
    #[tokio::test]
    #[ignore]
    async fn smtp_delivers_to_the_relay() {
        let url = std::env::var("SMTP_URL").expect("SMTP_URL");
        let mailer = SmtpMailer::from_url(&url, "no-reply@ruum.localhost").unwrap();
        let mut m = invitation(
            "https://ruum.localhost/invitations/accept?token=ruum_inv_check",
            Some(".a line that starts with a dot"),
        );
        m.to = "smtp-check@example.test".into();
        mailer.send(m).await.unwrap();
    }
}
