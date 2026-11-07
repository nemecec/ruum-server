-- The `type` filter of `searchNodes` (decision 0003-7, BENCHMARK fix 4).
--
-- `type` matches `nodes.type` (`Node.type` in the contract: the MIME type of a file, or the type
-- of a folder or typed node), ignoring case: an exact type (`application/pdf`) or a prefix that
-- ends in `/` (`image/`). `text_pattern_ops` lets the prefix match (`like 'image/%'`) use the
-- index. Only live nodes are searched.

create index nodes_type_idx on nodes (tenant_id, lower(type) text_pattern_ops)
    where trashed_at is null;
