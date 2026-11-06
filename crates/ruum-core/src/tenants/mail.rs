//! Outgoing mail (invitations). On the laptop the server sends to Mailpit (`SMTP_URL`,
//! `smtp://127.0.0.1:1025`); tests use [`MemoryMailer`].
//!
//! [`SmtpMailer`] is a small SMTP client for a relay that needs no TLS and no login (Mailpit, or
//! a local relay in production). A provider with TLS and login comes later.

use crate::error::{Error, Result};
use async_trait::async_trait;
use std::sync::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// One email.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mail {
    pub to: String,
    pub subject: String,
    /// Plain text.
    pub body: String,
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
        self.sent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(mail);
        Ok(())
    }
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
        let date = crate::now()
            .format(&time::format_description::well_known::Rfc2822)
            .unwrap_or_default();
        let mut msg = format!(
            "From: Ruum <{}>\r\nTo: <{}>\r\nSubject: {}\r\nDate: {date}\r\nMIME-Version: 1.0\r\n\
             Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n",
            self.from,
            mail.to,
            mail.subject.replace(['\r', '\n'], " ")
        );
        for line in mail.body.lines() {
            // Dot stuffing (RFC 5321, section 4.5.2).
            if line.starts_with('.') {
                msg.push('.');
            }
            msg.push_str(line);
            msg.push_str("\r\n");
        }
        msg.push_str(".\r\n");
        w.write_all(msg.as_bytes()).await?;
        expect(&mut r, 250).await?;
        w.write_all(b"QUIT\r\n").await?;
        Ok(())
    }
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
        self.deliver(&mail)
            .await
            .map_err(|e| Error::Internal(format!("send mail: {e}")))
    }
}

/// The invitation email.
pub fn invitation_mail(
    to: &str,
    tenant_name: &str,
    inviter: &str,
    accept_url: &str,
    message: Option<&str>,
) -> Mail {
    let mut body = format!(
        "{inviter} invited you to the Ruum space \"{tenant_name}\".\n\n\
         Open this link to accept the invitation:\n{accept_url}\n"
    );
    if let Some(m) = message.filter(|m| !m.trim().is_empty()) {
        body.push_str(&format!("\nMessage from {inviter}:\n{m}\n"));
    }
    body.push_str("\nIf you did not expect this email, you can ignore it.\n");
    Mail {
        to: to.to_string(),
        subject: format!("Invitation to {tenant_name} on Ruum"),
        body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_invitation_mail_has_the_link() {
        let m = invitation_mail("a@b.test", "Acme", "Mari", "https://x/accept?token=t", None);
        assert!(m.body.contains("https://x/accept?token=t"));
        assert!(m.subject.contains("Acme"));
    }

    /// Sends one mail to the relay of `SMTP_URL` (Mailpit on the laptop). Run with
    /// `cargo test -p ruum-core -- --ignored smtp`.
    #[tokio::test]
    #[ignore]
    async fn smtp_delivers_to_the_relay() {
        let url = std::env::var("SMTP_URL").expect("SMTP_URL");
        let mailer = SmtpMailer::from_url(&url, "no-reply@ruum.localhost").unwrap();
        mailer
            .send(invitation_mail(
                "smtp-check@example.test",
                "SMTP check",
                "ruum tests",
                "https://ruum.localhost/invitations/accept?token=ruum_inv_check",
                Some(".a line that starts with a dot"),
            ))
            .await
            .unwrap();
    }
}
