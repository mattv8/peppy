use crate::credentials::Binding;
use peppy_client_core::ConversationId;
use std::collections::HashMap;

#[derive(Clone)]
pub struct MovedScope {
    pub binding: Binding,
    pub conversation: ConversationId,
}

/// Resolves a composer request only against that window's locally observed move.
/// It deliberately never consults core's global forwarding records.
pub fn effective_conversation(
    source: ConversationId,
    moved: Option<&MovedScope>,
    binding: &Binding,
    requested: ConversationId,
) -> Option<ConversationId> {
    if let Some(scope) = moved.filter(|scope| &scope.binding == binding) {
        return (requested == source || requested == scope.conversation)
            .then_some(scope.conversation);
    }
    (requested == source).then_some(source)
}

pub fn clear_for_binding(scopes: &mut HashMap<String, MovedScope>, binding: &Binding) {
    scopes.retain(|_, scope| &scope.binding == binding);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> Binding {
        Binding {
            origin: "https://example.test".into(),
            vault_id: "vault".into(),
            device_id: "device".into(),
        }
    }

    #[test]
    fn effective_conversation_allows_only_source_or_its_mapped_target() {
        let source = ConversationId::new();
        let target = ConversationId::new();
        let unrelated = ConversationId::new();
        let binding = binding();
        let moved = MovedScope {
            binding: binding.clone(),
            conversation: target,
        };

        assert_eq!(
            effective_conversation(source, None, &binding, source),
            Some(source)
        );
        assert_eq!(
            effective_conversation(source, Some(&moved), &binding, source),
            Some(target)
        );
        assert_eq!(
            effective_conversation(source, Some(&moved), &binding, target),
            Some(target)
        );
        assert_eq!(
            effective_conversation(source, Some(&moved), &binding, unrelated),
            None
        );
        assert_eq!(effective_conversation(source, None, &binding, target), None);
        let other_binding = Binding {
            device_id: "other".into(),
            ..binding.clone()
        };
        assert_eq!(
            effective_conversation(source, Some(&moved), &other_binding, target),
            None
        );
    }
}
