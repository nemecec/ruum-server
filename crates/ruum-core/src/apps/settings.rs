//! The settings of a web app that are not in the archive (`AppSettings` of `openapi.yaml`;
//! `docs/STEP3.md`, "Permission table for web app nodes").
//!
//! Who may change what (the service checks `manage` on the app first; [`apply`] checks the rest):
//!
//! - `access: public` needs the platform admin's approval for the tenant
//!   (`Tenant.app_approval.public_apps`, decision 22): else `403 permission_denied`, hint
//!   `public_apps_not_allowed`. It also needs an owner or admin of the tenant (in a team tenant
//!   only they make an app public): else `403 permission_denied`, hint `tenant_admin_required`.
//! - An agent never changes `access` (`403 user_required`), never raises `telemetry_mode` to
//!   `full` and never raises `raw_copy_days` (full telemetry and the raw copy are a person's
//!   choice, design 9.6, review R9). An agent may lower both, and may change the report widget
//!   and the automatic rollback.
//!
//! `createApp` takes the same settings and goes through the same rules from the defaults.

use crate::error::{ApiError, Error, ErrorCode, Result};
use crate::tenants::MemberRole;
use serde::{Deserialize, Serialize};

/// `AppAccess`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    #[default]
    Platform,
    Public,
}

impl Access {
    pub fn as_str(self) -> &'static str {
        match self {
            Access::Platform => "platform",
            Access::Public => "public",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "platform" => Some(Access::Platform),
            "public" => Some(Access::Public),
            _ => None,
        }
    }
}

/// `AppSettings.telemetry_mode`. `Full` is more than `Privacy`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryMode {
    #[default]
    Privacy,
    Full,
}

impl TelemetryMode {
    pub fn as_str(self) -> &'static str {
        match self {
            TelemetryMode::Privacy => "privacy",
            TelemetryMode::Full => "full",
        }
    }
}

/// `AppSettings.report_widget.position`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WidgetPosition {
    #[default]
    BottomRight,
    BottomLeft,
    TopRight,
    TopLeft,
}

/// `AppSettings.report_widget`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportWidget {
    pub enabled: bool,
    pub position: WidgetPosition,
}

impl Default for ReportWidget {
    fn default() -> Self {
        Self {
            enabled: true,
            position: WidgetPosition::BottomRight,
        }
    }
}

/// `AppSettings.auto_rollback`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutoRollback {
    pub enabled: bool,
    pub error_rate_factor: f64,
}

impl Default for AutoRollback {
    fn default() -> Self {
        Self {
            enabled: true,
            error_rate_factor: 3.0,
        }
    }
}

/// The highest `raw_copy_days`.
pub const MAX_RAW_COPY_DAYS: u32 = 30;
/// The range of `auto_rollback.error_rate_factor`.
pub const ERROR_RATE_FACTOR: (f64, f64) = (1.5, 20.0);

/// The stored settings of an app, every field set (the defaults of the contract filled in).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AppSettings {
    pub access: Access,
    pub telemetry_mode: TelemetryMode,
    pub raw_copy_days: u32,
    pub report_widget: ReportWidget,
    pub auto_rollback: AutoRollback,
}

impl AppSettings {
    /// The settings of a stored JSON object; missing or unknown values take the defaults.
    pub fn from_stored(v: &serde_json::Value) -> Self {
        let mut s = AppSettings::default();
        if let Ok(input) = serde_json::from_value::<SettingsInput>(v.clone()) {
            s.merge(&input);
        }
        s
    }

    fn merge(&mut self, i: &SettingsInput) {
        if let Some(a) = i.access {
            self.access = a;
        }
        if let Some(t) = i.telemetry_mode {
            self.telemetry_mode = t;
        }
        if let Some(d) = i.raw_copy_days {
            self.raw_copy_days = d.clamp(0, MAX_RAW_COPY_DAYS as i64) as u32;
        }
        if let Some(w) = &i.report_widget {
            if let Some(e) = w.enabled {
                self.report_widget.enabled = e;
            }
            if let Some(p) = w.position {
                self.report_widget.position = p;
            }
        }
        if let Some(r) = &i.auto_rollback {
            if let Some(e) = r.enabled {
                self.auto_rollback.enabled = e;
            }
            if let Some(f) = r.error_rate_factor {
                self.auto_rollback.error_rate_factor = f;
            }
        }
    }
}

/// `AppSettings` of `createApp` and `UpdateAppSettingsRequest`: a missing property keeps its
/// value.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SettingsInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<Access>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry_mode: Option<TelemetryMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_copy_days: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_widget: Option<WidgetInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_rollback: Option<RollbackInput>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WidgetInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<WidgetPosition>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RollbackInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_rate_factor: Option<f64>,
}

impl SettingsInput {
    /// The names of the properties the input sets (`app.settings_changed.changed`).
    pub fn names(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.access.is_some() {
            v.push("access");
        }
        if self.telemetry_mode.is_some() {
            v.push("telemetry_mode");
        }
        if self.raw_copy_days.is_some() {
            v.push("raw_copy_days");
        }
        if self.report_widget.is_some() {
            v.push("report_widget");
        }
        if self.auto_rollback.is_some() {
            v.push("auto_rollback");
        }
        v
    }

    pub fn is_empty(&self) -> bool {
        self.names().is_empty()
    }
}

/// Who changes the settings, as [`apply`] needs it.
#[derive(Clone, Copy, Debug)]
pub struct Changer {
    pub is_agent: bool,
    /// The tenant role of the effective user (for an agent: its user).
    pub tenant_role: Option<MemberRole>,
    /// `Tenant.app_approval.public_apps`.
    pub public_apps_approved: bool,
}

fn invalid(message: &str, pointer: &str) -> Error {
    Error::Api(ApiError::new(ErrorCode::ValidationFailed, message).with_hint(pointer))
}

/// `403 permission_denied`, hint `public_apps_not_allowed`.
pub fn public_apps_not_allowed() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::PermissionDenied,
            "public apps are not approved for this space; a Ruum admin approves them",
        )
        .with_hint("public_apps_not_allowed"),
    )
}

/// `403 permission_denied`, hint `tenant_admin_required`.
pub fn tenant_admin_required() -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::PermissionDenied,
            "only an owner or admin of the space makes an app public",
        )
        .with_hint("tenant_admin_required"),
    )
}

/// `403 user_required` for a change that only a person makes.
fn person_only(what: &str) -> Error {
    Error::Api(
        ApiError::new(
            ErrorCode::UserRequired,
            format!("only a person can {what}; agents cannot"),
        )
        .with_hint("ask the user to do it in the web UI"),
    )
}

/// The checks of the values alone (ranges), before any rule of who.
pub fn check_values(input: &SettingsInput) -> Result<()> {
    if let Some(d) = input.raw_copy_days
        && !(0..=MAX_RAW_COPY_DAYS as i64).contains(&d)
    {
        return Err(invalid("raw_copy_days is 0 to 30", "/raw_copy_days"));
    }
    if let Some(f) = input
        .auto_rollback
        .as_ref()
        .and_then(|r| r.error_rate_factor)
        && !(f.is_finite() && (ERROR_RATE_FACTOR.0..=ERROR_RATE_FACTOR.1).contains(&f))
    {
        return Err(invalid(
            "auto_rollback.error_rate_factor is 1.5 to 20",
            "/auto_rollback/error_rate_factor",
        ));
    }
    Ok(())
}

/// The new settings after `input`, or the refusal (the rules of the module docs). The caller
/// has checked `manage` on the app (or `edit` on the parent for a new app).
pub fn apply(current: &AppSettings, input: &SettingsInput, who: Changer) -> Result<AppSettings> {
    check_values(input)?;
    let mut next = *current;
    next.merge(input);
    if who.is_agent {
        // An agent never sets access (also not the value it has), so it can never make an app
        // public, nor take part in the decision.
        if input.access.is_some() {
            return Err(person_only("change who may use an app"));
        }
        if next.telemetry_mode > current.telemetry_mode {
            return Err(person_only("turn on full telemetry"));
        }
        if next.raw_copy_days > current.raw_copy_days {
            return Err(person_only("keep raw copies of telemetry longer"));
        }
    }
    if next.access == Access::Public && current.access != Access::Public {
        if !who.public_apps_approved {
            return Err(public_apps_not_allowed());
        }
        if !matches!(who.tenant_role, Some(MemberRole::Owner | MemberRole::Admin)) {
            return Err(tenant_admin_required());
        }
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn input(v: serde_json::Value) -> SettingsInput {
        serde_json::from_value(v).unwrap()
    }

    fn person(role: MemberRole, approved: bool) -> Changer {
        Changer {
            is_agent: false,
            tenant_role: Some(role),
            public_apps_approved: approved,
        }
    }

    fn agent() -> Changer {
        Changer {
            is_agent: true,
            tenant_role: Some(MemberRole::Owner),
            public_apps_approved: true,
        }
    }

    fn hint(r: Result<AppSettings>) -> (ErrorCode, Option<String>) {
        match r {
            Err(Error::Api(e)) => (e.code, e.hint),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn defaults_follow_the_contract() {
        let s = AppSettings::default();
        assert_eq!(
            serde_json::to_value(s).unwrap(),
            json!({
                "access": "platform",
                "telemetry_mode": "privacy",
                "raw_copy_days": 0,
                "report_widget": { "enabled": true, "position": "bottom_right" },
                "auto_rollback": { "enabled": true, "error_rate_factor": 3.0 },
            })
        );
        assert_eq!(AppSettings::from_stored(&json!({})), s);
        let stored = serde_json::to_value(s).unwrap();
        assert_eq!(AppSettings::from_stored(&stored), s);
    }

    #[test]
    fn public_needs_the_approval_and_an_owner_or_admin() {
        let cur = AppSettings::default();
        let public = input(json!({ "access": "public" }));
        // Without the platform admin's approval: refused, also for an owner.
        assert_eq!(
            hint(apply(&cur, &public, person(MemberRole::Owner, false))),
            (
                ErrorCode::PermissionDenied,
                Some("public_apps_not_allowed".into())
            )
        );
        // A member of a team tenant: refused even with the approval.
        assert_eq!(
            hint(apply(&cur, &public, person(MemberRole::Member, true))),
            (
                ErrorCode::PermissionDenied,
                Some("tenant_admin_required".into())
            )
        );
        assert_eq!(
            hint(apply(&cur, &public, person(MemberRole::Guest, true))).1,
            Some("tenant_admin_required".into())
        );
        // An owner or admin with the approval: allowed.
        for role in [MemberRole::Owner, MemberRole::Admin] {
            let s = apply(&cur, &public, person(role, true)).unwrap();
            assert_eq!(s.access, Access::Public);
        }
        // Keeping an app public (another field) needs no new decision.
        let already = AppSettings {
            access: Access::Public,
            ..cur
        };
        let w = input(json!({ "report_widget": { "enabled": false } }));
        assert!(apply(&already, &w, person(MemberRole::Member, false)).is_ok());
        // Going back to platform is always allowed for a person.
        let back = input(json!({ "access": "platform" }));
        assert!(apply(&already, &back, person(MemberRole::Member, false)).is_ok());
    }

    #[test]
    fn an_agent_never_sets_access_nor_raises_telemetry() {
        let cur = AppSettings::default();
        for v in [
            json!({ "access": "public" }),
            json!({ "access": "platform" }),
            json!({ "telemetry_mode": "full" }),
            json!({ "raw_copy_days": 1 }),
        ] {
            assert_eq!(
                hint(apply(&cur, &input(v.clone()), agent())).0,
                ErrorCode::UserRequired,
                "{v}"
            );
        }
        // It may lower both, keep them, and change the widget and the rollback.
        let high = AppSettings {
            telemetry_mode: TelemetryMode::Full,
            raw_copy_days: 7,
            ..cur
        };
        let s = apply(
            &high,
            &input(json!({ "telemetry_mode": "privacy", "raw_copy_days": 3 })),
            agent(),
        )
        .unwrap();
        assert_eq!(
            (s.telemetry_mode, s.raw_copy_days),
            (TelemetryMode::Privacy, 3)
        );
        assert!(apply(&high, &input(json!({ "telemetry_mode": "full" })), agent()).is_ok());
        assert_eq!(
            hint(apply(&high, &input(json!({ "raw_copy_days": 8 })), agent())).0,
            ErrorCode::UserRequired
        );
        let s = apply(
            &cur,
            &input(json!({ "report_widget": { "position": "top_left" }, "auto_rollback": { "enabled": false } })),
            agent(),
        )
        .unwrap();
        assert_eq!(s.report_widget.position, WidgetPosition::TopLeft);
        assert!(s.report_widget.enabled);
        assert!(!s.auto_rollback.enabled);
        // A person raises both.
        let s = apply(
            &cur,
            &input(json!({ "telemetry_mode": "full", "raw_copy_days": 30 })),
            person(MemberRole::Member, false),
        )
        .unwrap();
        assert_eq!(
            (s.telemetry_mode, s.raw_copy_days),
            (TelemetryMode::Full, 30)
        );
    }

    #[test]
    fn values_out_of_range_are_422_with_a_pointer() {
        let cur = AppSettings::default();
        let p = person(MemberRole::Owner, true);
        for (v, ptr) in [
            (json!({ "raw_copy_days": 31 }), "/raw_copy_days"),
            (json!({ "raw_copy_days": -1 }), "/raw_copy_days"),
            (
                json!({ "auto_rollback": { "error_rate_factor": 1.0 } }),
                "/auto_rollback/error_rate_factor",
            ),
            (
                json!({ "auto_rollback": { "error_rate_factor": 21 } }),
                "/auto_rollback/error_rate_factor",
            ),
        ] {
            assert_eq!(
                hint(apply(&cur, &input(v), p)),
                (ErrorCode::ValidationFailed, Some(ptr.into()))
            );
        }
        assert_eq!(
            input(json!({ "access": "public", "raw_copy_days": 2 })).names(),
            ["access", "raw_copy_days"]
        );
        assert!(input(json!({})).is_empty());
    }
}
