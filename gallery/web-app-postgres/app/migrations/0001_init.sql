-- The first migration of the template: one table of notes.
-- ruum:classes public.notes.id=normal public.notes.author_sub=personal
-- ruum:classes public.notes.author_name=personal public.notes.title=normal
-- ruum:classes public.notes.body=personal public.notes.created_at=normal
create table notes (
    id bigint generated always as identity primary key,
    author_sub text not null,
    author_name text,
    title text not null check (length(title) between 1 and 200),
    body text not null default '' check (length(body) <= 10000),
    created_at timestamptz not null default now()
);

create index notes_author_sub_created_at on notes (author_sub, created_at desc);
