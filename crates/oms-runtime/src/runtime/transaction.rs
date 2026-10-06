#[derive(Debug, Clone)]
enum Operation {
    Create(CreateObject),
    UpdateState {
        object: ObjectId,
        state: Vec<u8>,
    },
    SetLink {
        source: ObjectId,
        name: String,
        target: ObjectId,
    },
    RemoveLink {
        source: ObjectId,
        name: String,
    },
    Reparent {
        child: ObjectId,
        new_parent: Option<ObjectId>,
    },
    Grant {
        object: ObjectId,
        subject: SubjectId,
        capability: Capability,
    },
    Revoke {
        object: ObjectId,
        subject: SubjectId,
        capability: Capability,
    },
    Tombstone {
        object: ObjectId,
    },
}

#[derive(Debug, Clone)]
pub struct Transaction {
    id: TransactionId,
    context: AccessContext,
    expected: BTreeMap<ObjectId, ObjectVersion>,
    operations: Vec<Operation>,
}

impl Transaction {
    #[must_use]
    pub fn new(context: AccessContext) -> Self {
        Self {
            id: TransactionId::new(),
            context,
            expected: BTreeMap::new(),
            operations: Vec::new(),
        }
    }

    #[must_use]
    pub const fn id(&self) -> TransactionId {
        self.id
    }

    pub fn expect(&mut self, object: ObjectId, version: ObjectVersion) -> &mut Self {
        self.expected.insert(object, version);
        self
    }

    pub fn create(&mut self, request: CreateObject) -> &mut Self {
        self.operations.push(Operation::Create(request));
        self
    }

    pub fn update_state(&mut self, object: ObjectId, state: impl Into<Vec<u8>>) -> &mut Self {
        self.operations.push(Operation::UpdateState {
            object,
            state: state.into(),
        });
        self
    }

    pub fn set_link(
        &mut self,
        source: ObjectId,
        name: impl Into<String>,
        target: ObjectId,
    ) -> &mut Self {
        self.operations.push(Operation::SetLink {
            source,
            name: name.into(),
            target,
        });
        self
    }

    pub fn remove_link(&mut self, source: ObjectId, name: impl Into<String>) -> &mut Self {
        self.operations.push(Operation::RemoveLink {
            source,
            name: name.into(),
        });
        self
    }

    pub fn reparent(&mut self, child: ObjectId, new_parent: Option<ObjectId>) -> &mut Self {
        self.operations
            .push(Operation::Reparent { child, new_parent });
        self
    }

    pub fn grant(
        &mut self,
        object: ObjectId,
        subject: SubjectId,
        capability: Capability,
    ) -> &mut Self {
        self.operations.push(Operation::Grant {
            object,
            subject,
            capability,
        });
        self
    }

    pub fn revoke(
        &mut self,
        object: ObjectId,
        subject: SubjectId,
        capability: Capability,
    ) -> &mut Self {
        self.operations.push(Operation::Revoke {
            object,
            subject,
            capability,
        });
        self
    }

    pub fn tombstone(&mut self, object: ObjectId) -> &mut Self {
        self.operations.push(Operation::Tombstone { object });
        self
    }
}
