-- Email grants that bind (decision 0003-2, contract need C-12) and share and invitation emails
-- (decision 0003-3, contract need C-13).
--
-- 1. `grants_email_lookup_idx`: the bind looks up the tenants that hold email grants for one
--    address (a system lookup), and the exists check of `getMe` and `listSharedWithMe` uses
--    the same index.
-- 2. `sessions.bind_checked_at`: the last bind check of a web session. `getMe` and
--    `listSharedWithMe` check at most once per session per 60 seconds; the item page of a share
--    email forces one check (`listSharedWithMe?check_email_grants=true`).
-- 3. `rate_counters`: fixed-window counters in Postgres, shared by every server process
--    (`addGrant`: 60 per 10 minutes per user and tenant, 600 per tenant; share mails: 50 per
--    hour per user and tenant, 500 per tenant). The mail worker deletes old windows.
-- 4. `mail_outbox`: mails written in the transaction of the change (a grant, an invitation) and
--    sent by the mail worker after the commit. A row holds no secret: an invitation row has the
--    invitation ID and the worker mints the token when it sends. `payload` is cleared when the
--    mail is sent or the worker gives up; the worker deletes sent and given-up rows after 7
--    days (DESIGN 15: the rows hold email addresses and the personal message). The GDPR
--    deletion job (not built yet) must delete a user's rows.
-- 5. `invitation_tokens`: the hashes of invitation tokens, one row per token, so a retry or a
--    resend mints a new token without ending an earlier one. A token works only while its
--    invitation is pending (not accepted, revoked or expired). The worker deletes the rows of
--    an ended invitation after 7 days; the invitation row keeps the state.
--    `invitations.token_hash` is copied here and is no longer written.

create index grants_email_lookup_idx on grants (lower(grantee_email)) where grantee_kind = 'email';

alter table sessions add column bind_checked_at timestamptz;

create table rate_counters (
    tenant_id    uuid not null references tenants (id),
    key          text not null,
    window_start timestamptz not null,
    count        integer not null default 0,
    primary key (tenant_id, key, window_start)
);
create index rate_counters_window_idx on rate_counters (window_start);

create table mail_outbox (
    tenant_id       uuid not null references tenants (id),
    id              uuid primary key,
    kind            text not null check (kind in ('share', 'invitation')),
    to_email        text not null,
    -- Share mails: the node of the grant (the worker checks that a grant for the recipient
    -- still exists). Invitation mails: the invitation.
    node_id         uuid,
    invitation_id   uuid references invitations (id),
    -- The user (or the agent's user) who caused the mail, for the per-user limit.
    sender_user_id  uuid,
    -- Share mails: the rendered mail. Invitation mails: the fields of the mail without the
    -- link (the worker adds the link with a new token). Null after sending or giving up.
    payload         jsonb,
    created_at      timestamptz not null default now(),
    next_attempt_at timestamptz not null default now(),
    attempts        integer not null default 0,
    sent_at         timestamptz,
    given_up_at     timestamptz,
    dropped_at      timestamptz,
    last_error      text,
    check ((kind = 'share') = (node_id is not null)),
    check ((kind = 'invitation') = (invitation_id is not null))
);
create index mail_outbox_pending_idx on mail_outbox (next_attempt_at)
    where sent_at is null and given_up_at is null and dropped_at is null;
create index mail_outbox_recent_idx on mail_outbox (tenant_id, lower(to_email), node_id, created_at desc);
create index mail_outbox_sender_idx on mail_outbox (tenant_id, sender_user_id, created_at desc);

create table invitation_tokens (
    tenant_id     uuid not null references tenants (id),
    invitation_id uuid not null references invitations (id),
    token_hash    bytea not null unique,
    created_at    timestamptz not null default now()
);
create index invitation_tokens_invitation_idx on invitation_tokens (invitation_id);

insert into invitation_tokens (tenant_id, invitation_id, token_hash, created_at)
    select tenant_id, id, token_hash, created_at from invitations where token_hash is not null;
alter table invitations alter column token_hash drop not null;

do $$
declare
    t text;
begin
    foreach t in array array['rate_counters', 'mail_outbox', 'invitation_tokens'] loop
        execute format('alter table %I enable row level security', t);
        execute format('alter table %I force row level security', t);
        execute format(
            'create policy tenant_isolation on %I
                 using (tenant_id = ruum_current_tenant() or ruum_is_system())
                 with check (tenant_id = ruum_current_tenant() or ruum_is_system())', t);
    end loop;
end
$$;
