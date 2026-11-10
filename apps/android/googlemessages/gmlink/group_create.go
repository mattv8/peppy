package gmlink

import (
	"context"
	"encoding/json"
	"slices"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

type groupCreateRequest struct {
	Numbers []string `json:"numbers"`
	Name    string   `json:"name"`
}

type groupCreateResult struct {
	Code                 string `json:"code"`
	GoogleConversationID string `json:"google_conversation_id,omitempty"`
}

// CreateRcsGroup performs Google's documented two-step group creation exactly once.
func (link *Link) CreateRcsGroup(requestJSON string) string {
	var request groupCreateRequest
	if !decodeGroupCreateRequest(requestJSON, &request) {
		return groupCreateJSON(groupCreateResult{Code: "unknown"})
	}
	wanted, ok := groupRequestSet(request.Numbers)
	if !ok {
		return groupCreateJSON(groupCreateResult{Code: "not_rcs"})
	}

	link.mu.Lock()
	client, runCtx := link.client, link.runCtx
	allowed := !link.closed && link.running && link.runGeneration == link.generation && link.hasGroupsVerificationLocked() && link.conversationIndexCurrentLocked()
	link.mu.Unlock()
	if !allowed || client == nil || runCtx == nil {
		return groupCreateJSON(groupCreateResult{Code: "unknown"})
	}
	ctx, cancel := context.WithTimeout(runCtx, rcsSendDeadline)
	defer cancel()
	if !link.lockRPC(ctx) {
		return groupCreateJSON(groupCreateResult{Code: "unknown"})
	}
	defer link.rpcMu.Unlock()
	var name *string
	if request.Name != "" {
		name = &request.Name
	}
	return runGroupCreate(ctx, client.GetOrCreateConversation, wanted, name)
}

// getOrCreateCall is the one Google RPC the two-step create uses; tests substitute it.
type getOrCreateCall func(context.Context, *gmproto.GetOrCreateConversationRequest) (*gmproto.GetOrCreateConversationResponse, error)

// runGroupCreate performs the two-step exchange exactly once: a lookup, then a create only when
// Google answers CREATE_RCS. A missing name is an unnamed group and is sent as an absent field.
func runGroupCreate(ctx context.Context, call getOrCreateCall, wanted []string, name *string) string {
	numbers := make([]*gmproto.ContactNumber, len(wanted))
	for index, number := range wanted {
		numbers[index] = &gmproto.ContactNumber{MysteriousInt: 2, Number: number, Number2: number}
	}
	response, err := call(ctx, &gmproto.GetOrCreateConversationRequest{Numbers: numbers})
	if err != nil || response == nil || ctx.Err() != nil {
		return groupCreateJSON(groupCreateResult{Code: "unknown"})
	}
	if response.GetStatus() == gmproto.GetOrCreateConversationResponse_SUCCESS {
		return groupCreateClassification(response.GetConversation(), wanted, "existing_rcs_group")
	}
	if response.GetStatus() != gmproto.GetOrCreateConversationResponse_CREATE_RCS {
		return groupCreateJSON(groupCreateResult{Code: "not_rcs"})
	}
	create := true
	response, err = call(ctx, &gmproto.GetOrCreateConversationRequest{Numbers: numbers, RCSGroupName: name, CreateRCSGroup: &create})
	if err != nil || response == nil || ctx.Err() != nil {
		return groupCreateJSON(groupCreateResult{Code: "unknown"})
	}
	return groupCreateClassification(response.GetConversation(), wanted, "created_rcs_group")
}

// decodeGroupCreateRequest accepts exactly the fields numbers and name. Name is optional: absent or
// empty means unnamed. A nonempty name must be bounded; unknown fields and null name are refused.
func decodeGroupCreateRequest(input string, output *groupCreateRequest) bool {
	if len(input) == 0 || len(input) > maxPrepareJSONBytes || json.Unmarshal([]byte(input), output) != nil || (output.Name != "" && !boundedGroupString(output.Name)) {
		return false
	}
	var fields map[string]json.RawMessage
	if json.Unmarshal([]byte(input), &fields) != nil || fields["numbers"] == nil {
		return false
	}
	for key, value := range fields {
		if key == "numbers" {
			continue
		}
		if key != "name" || string(value) == "null" {
			return false
		}
	}
	return true
}

func groupCreateClassification(conversation *gmproto.Conversation, wanted []string, code string) string {
	if conversation == nil || conversation.GetType() != gmproto.ConversationType_RCS {
		return groupCreateJSON(groupCreateResult{Code: "not_rcs"})
	}
	projection, result := projectGroup(conversation)
	if result != groupResolved || !slices.Equal(projection.CanonicalAddresses, wanted) {
		return groupCreateJSON(groupCreateResult{Code: "not_rcs"})
	}
	return groupCreateJSON(groupCreateResult{Code: code, GoogleConversationID: projection.ConversationID})
}

func groupCreateJSON(result groupCreateResult) string {
	encoded, _ := json.Marshal(result)
	return string(encoded)
}
