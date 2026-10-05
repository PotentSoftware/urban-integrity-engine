pub mod attribution;
pub mod edge;
pub mod id;
pub mod node;
pub mod verdict;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn test_node_serialization() {
        let node = node::NodeWeight::Entity(node::EntityData {
            name: "Test Entity".to_string(),
            company_number: None,
            status: node::EntityStatus::Active,
            incorporation_date: None,
        });

        let serialized = serde_json::to_string(&node).unwrap();
        assert!(serialized.contains(r#""type":"Entity""#));

        let deserialized: node::NodeWeight = serde_json::from_str(&serialized).unwrap();
        assert_eq!(node, deserialized);
    }

    #[test]
    fn test_edge_serialization() {
        let edge = edge::EdgeWeight::Directs {
            meta: edge::EdgeMeta {
                created_at: Utc::now(),
                effective_from: None,
                effective_to: None,
            },
            data: edge::DirectsData {
                details: "Director role".to_string(),
                officer_role: None,
            },
        };

        let serialized = serde_json::to_string(&edge).unwrap();
        assert!(serialized.contains(r#""type":"Directs""#));

        let deserialized: edge::EdgeWeight = serde_json::from_str(&serialized).unwrap();
        assert_eq!(edge, deserialized);
    }
}
