//! In-memory relationship engine, for unit tests and for running without Docker.
//!
//! It reads the contract schema (`contracts/authz/schema.zed`) with [`Schema`] and evaluates
//! checks on it, so it follows the same rules as SpiceDB. It passes every assertion of
//! `contracts/authz/schema.test.yaml` and the conformance suite of [`Authz`]. Writes are checked
//! against the schema like SpiceDB does (the relation exists and allows the subject type).

use super::schema::{AllowedSubject, Expr, Schema};
use super::*;
use crate::error::Error;
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

/// The deepest recursion of one check. SpiceDB uses 50 as its default as well.
const MAX_DEPTH: u32 = 50;

#[derive(Debug, Default)]
struct State {
    /// `(object, relation)` -> subjects.
    tuples: HashMap<(ObjectRef, String), HashSet<SubjectRef>>,
    revision: u64,
}

/// The in-memory engine.
#[derive(Debug)]
pub struct MemoryAuthz {
    schema: Schema,
    state: RwLock<State>,
}

impl Default for MemoryAuthz {
    fn default() -> Self {
        Self::new(Schema::contract())
    }
}

impl MemoryAuthz {
    /// An engine for another schema (tests).
    pub fn new(schema: Schema) -> Self {
        Self {
            schema,
            state: RwLock::new(State::default()),
        }
    }

    /// All relationships, for tests and debugging.
    pub fn all(&self) -> Vec<Relationship> {
        let st = self.state.read().expect("authz lock");
        let mut v: Vec<Relationship> = st
            .tuples
            .iter()
            .flat_map(|((o, r), subs)| {
                subs.iter().map(move |s| Relationship {
                    object: o.clone(),
                    relation: r.clone(),
                    subject: s.clone(),
                })
            })
            .collect();
        v.sort_by_key(|r| r.to_string());
        v
    }

    fn validate(&self, r: &Relationship) -> Result<()> {
        let def = self
            .schema
            .definitions
            .get(&r.object.object_type)
            .ok_or_else(|| invalid(r, "unknown object type"))?;
        let allowed = def
            .relations
            .get(&r.relation)
            .ok_or_else(|| invalid(r, "unknown relation"))?;
        let s = &r.subject;
        let ok = allowed.iter().any(|a| match (a, &s.relation) {
            (AllowedSubject::Object(t), None) => {
                *t == s.object.object_type && s.object.object_id != "*"
            }
            (AllowedSubject::Wildcard(t), None) => {
                *t == s.object.object_type && s.object.object_id == "*"
            }
            (AllowedSubject::Set(t, rel), Some(sr)) => *t == s.object.object_type && rel == sr,
            _ => false,
        });
        if ok {
            Ok(())
        } else {
            Err(invalid(r, "subject type not allowed"))
        }
    }

    fn eval(
        &self,
        st: &State,
        object: &ObjectRef,
        name: &str,
        subject: &SubjectRef,
        depth: u32,
    ) -> Result<bool> {
        if depth > MAX_DEPTH {
            return Err(Error::Internal("authz: check is too deep".into()));
        }
        let Some(def) = self.schema.definitions.get(&object.object_type) else {
            return Ok(false);
        };
        if def.relations.contains_key(name) {
            // A subject set that is the subject itself (`tenant:T#member` asked directly).
            let Some(subs) = st.tuples.get(&(object.clone(), name.to_string())) else {
                return Ok(false);
            };
            for s in subs {
                if s == subject {
                    return Ok(true);
                }
                if s.relation.is_none()
                    && s.object.object_id == "*"
                    && s.object.object_type == subject.object.object_type
                    && subject.relation.is_none()
                {
                    return Ok(true);
                }
                if let Some(rel) = &s.relation
                    && self.eval(st, &s.object, rel, subject, depth + 1)?
                {
                    return Ok(true);
                }
            }
            Ok(false)
        } else if let Some(expr) = def.permissions.get(name) {
            self.expr(st, object, expr, subject, depth + 1)
        } else {
            Ok(false)
        }
    }

    fn expr(
        &self,
        st: &State,
        object: &ObjectRef,
        e: &Expr,
        subject: &SubjectRef,
        depth: u32,
    ) -> Result<bool> {
        Ok(match e {
            Expr::Ref(n) => self.eval(st, object, n, subject, depth)?,
            Expr::Arrow(rel, perm) => {
                let Some(subs) = st.tuples.get(&(object.clone(), rel.clone())) else {
                    return Ok(false);
                };
                for s in subs {
                    if self.eval(st, &s.object, perm, subject, depth + 1)? {
                        return Ok(true);
                    }
                }
                false
            }
            Expr::Union(a, b) => {
                self.expr(st, object, a, subject, depth)?
                    || self.expr(st, object, b, subject, depth)?
            }
            Expr::Intersect(a, b) => {
                self.expr(st, object, a, subject, depth)?
                    && self.expr(st, object, b, subject, depth)?
            }
            Expr::Exclude(a, b) => {
                self.expr(st, object, a, subject, depth)?
                    && !self.expr(st, object, b, subject, depth)?
            }
        })
    }

    fn check_now(
        &self,
        subject: &SubjectRef,
        permission: &str,
        object: &ObjectRef,
    ) -> Result<bool> {
        if !self.schema.has_name(&object.object_type, permission) {
            return Err(Error::Internal(format!(
                "authz: {}#{permission} is not in the schema",
                object.object_type
            )));
        }
        let st = self.state.read().expect("authz lock");
        self.eval(&st, object, permission, subject, 0)
    }
}

fn invalid(r: &Relationship, why: &str) -> Error {
    Error::Internal(format!("authz: invalid relationship {r}: {why}"))
}

#[async_trait]
impl Authz for MemoryAuthz {
    async fn check(
        &self,
        subject: &SubjectRef,
        permission: &str,
        object: &ObjectRef,
        _at_least: Option<&ConsistencyToken>,
    ) -> Result<bool> {
        self.check_now(subject, permission, object)
    }

    async fn check_bulk(
        &self,
        subject: &SubjectRef,
        permission: &str,
        objects: &[ObjectRef],
    ) -> Result<Vec<bool>> {
        objects
            .iter()
            .map(|o| self.check_now(subject, permission, o))
            .collect()
    }

    async fn write(&self, updates: Vec<RelationshipUpdate>) -> Result<ConsistencyToken> {
        for u in &updates {
            let (RelationshipUpdate::Touch(r) | RelationshipUpdate::Delete(r)) = u;
            self.validate(r)?;
        }
        let mut st = self.state.write().expect("authz lock");
        for u in updates {
            match u {
                RelationshipUpdate::Touch(r) => {
                    st.tuples
                        .entry((r.object, r.relation))
                        .or_default()
                        .insert(r.subject);
                }
                RelationshipUpdate::Delete(r) => {
                    let key = (r.object, r.relation);
                    if let Some(set) = st.tuples.get_mut(&key) {
                        set.remove(&r.subject);
                        if set.is_empty() {
                            st.tuples.remove(&key);
                        }
                    }
                }
            }
        }
        st.revision += 1;
        Ok(ConsistencyToken(format!("mem:{}", st.revision)))
    }

    async fn read(&self, object: &ObjectRef) -> Result<Vec<Relationship>> {
        let st = self.state.read().expect("authz lock");
        let mut v: Vec<Relationship> = st
            .tuples
            .iter()
            .filter(|((o, _), _)| o == object)
            .flat_map(|((o, r), subs)| {
                subs.iter().map(move |s| Relationship {
                    object: o.clone(),
                    relation: r.clone(),
                    subject: s.clone(),
                })
            })
            .collect();
        v.sort_by_key(|r| r.to_string());
        Ok(v)
    }
}
