package gmlink

import "encoding/json"

type conversationEvent struct {
	Code                 string `json:"code"`
	GoogleConversationID string `json:"google_conversation_id"`
}

func conversationJSON(code string) string {
	encoded, _ := json.Marshal(struct {
		Code string `json:"code"`
	}{Code: code})
	return string(encoded)
}

func (link *Link) recordConversationEvent(conversation interface{ GetConversationID() string }) {
	if conversation.GetConversationID() == "" {
		return
	}
	link.mu.Lock()
	defer link.mu.Unlock()
	if link.closed || !link.m2VerificationPassed {
		return
	}
	link.conversationEvents = append(link.conversationEvents, conversationEvent{
		Code:                 "conversation_updated",
		GoogleConversationID: conversation.GetConversationID(),
	})
}
