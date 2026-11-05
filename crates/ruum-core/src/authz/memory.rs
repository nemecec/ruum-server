//! In-memory relationship engine, for unit tests. It must follow the same rules as
//! `contracts/authz/schema.zed`. Not implemented yet.

use super::*;

#[derive(Debug, Default)]
pub struct MemoryAuthz {}

#[async_trait]
impl Authz for MemoryAuthz {
    async fn check(
        &self,
        _subject: &SubjectRef,
        _permission: &str,
        _object: &ObjectRef,
        _at_least: Option<&ConsistencyToken>,
    ) -> Result<bool> {
        todo!("authz: check")
    }
    async fn check_bulk(
        &self,
        _subject: &SubjectRef,
        _permission: &str,
        _objects: &[ObjectRef],
    ) -> Result<Vec<bool>> {
        todo!("authz: check_bulk")
    }
    async fn write(&self, _updates: Vec<RelationshipUpdate>) -> Result<ConsistencyToken> {
        todo!("authz: write")
    }
    async fn read(&self, _object: &ObjectRef) -> Result<Vec<Relationship>> {
        todo!("authz: read")
    }
}
