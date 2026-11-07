-- The language preference of a user (design 12.4, `updateMyPreferences`).
--
-- A BCP 47 tag (`et`, `en-GB`) or null to follow the browser. The web UI reads it from `getMe`;
-- later the server uses it for emails and invitations to the user. The row stays under the RLS
-- policy of `users`: only the user (and system code) writes it, and only `getMe` of the user
-- returns it.

alter table users add column locale text;
alter table users add constraint users_locale_check
    check (locale is null or (length(locale) <= 35
        and locale ~ '^[a-z]{2,3}(-[A-Z][a-z]{3})?(-([A-Z]{2}|[0-9]{3}))?$'));
