use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Hash, Eq, PartialEq)]
pub struct EntityId(pub String);

#[derive(Serialize, Deserialize, Clone, Debug, Hash, Eq, PartialEq)]
pub struct OfficerId(pub String);

#[derive(Serialize, Deserialize, Clone, Debug, Hash, Eq, PartialEq)]
pub struct LocationId(pub String);

#[derive(Serialize, Deserialize, Clone, Debug, Hash, Eq, PartialEq)]
pub struct ContractId(pub String);

#[derive(Serialize, Deserialize, Clone, Debug, Hash, Eq, PartialEq)]
pub struct WardId(pub String);

#[derive(Serialize, Deserialize, Clone, Debug, Hash, Eq, PartialEq)]
pub struct PoliticalActorId(pub String);
