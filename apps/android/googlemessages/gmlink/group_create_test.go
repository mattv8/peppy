package gmlink

import (
	"context"
	"errors"
	"strings"
	"testing"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

// Exact request JSON that the Kotlin coordinator produces (RcsGroupCreateCoordinator.createRequestJson).
const (
	kotlinNamedCreateRequest   = `{"numbers":["+15551230001","+15551230002"],"name":"Weekend"}`
	kotlinUnnamedCreateRequest = `{"numbers":["+15551230001","+15551230002"]}`
)

func TestGroupCreateRequestDecodesKotlinNamedAndUnnamedJSON(t *testing.T) {
	for _, tc := range []struct {
		name  string
		input string
		want  string
	}{
		{name: "named", input: kotlinNamedCreateRequest, want: "Weekend"},
		{name: "unnamed", input: kotlinUnnamedCreateRequest, want: ""},
		{name: "empty name is unnamed", input: `{"numbers":["+15551230001","+15551230002"],"name":""}`, want: ""},
	} {
		var request groupCreateRequest
		if !decodeGroupCreateRequest(tc.input, &request) || request.Name != tc.want || len(request.Numbers) != 2 {
			t.Fatalf("%s: decode(%s) = %+v", tc.name, tc.input, request)
		}
	}
	for _, input := range []string{
		`{"numbers":["+15551230001","+15551230002"],"name":"Weekend","extra":true}`,
		`{"numbers":["+15551230001","+15551230002"],"name":null}`,
		`{"numbers":["+15551230001","+15551230002"],"name":"` + strings.Repeat("n", groupStringLimit+1) + `"}`,
		`{"name":"Weekend"}`,
		``,
	} {
		var request groupCreateRequest
		if decodeGroupCreateRequest(input, &request) {
			t.Fatalf("decode(%q) accepted an invalid request", input)
		}
	}
}

func createTestConversation() *gmproto.Conversation {
	conversation := validGroupConversation()
	conversation.Participants[0].ID.Number = "+15551230001"
	conversation.Participants[1].ID.Number = "+15551230002"
	return conversation
}

// scriptedCreate answers the two-step RPC from a script and records each request it receives.
func scriptedCreate(answers ...func() (*gmproto.GetOrCreateConversationResponse, error)) (getOrCreateCall, *[]*gmproto.GetOrCreateConversationRequest) {
	var calls []*gmproto.GetOrCreateConversationRequest
	call := func(_ context.Context, request *gmproto.GetOrCreateConversationRequest) (*gmproto.GetOrCreateConversationResponse, error) {
		calls = append(calls, request)
		if len(calls) > len(answers) {
			return nil, errors.New("unexpected call")
		}
		return answers[len(calls)-1]()
	}
	return call, &calls
}

func answerStatus(status gmproto.GetOrCreateConversationResponse_Status, conversation *gmproto.Conversation) func() (*gmproto.GetOrCreateConversationResponse, error) {
	return func() (*gmproto.GetOrCreateConversationResponse, error) {
		return &gmproto.GetOrCreateConversationResponse{Status: status, Conversation: conversation}, nil
	}
}

func answerError() (*gmproto.GetOrCreateConversationResponse, error) {
	return nil, errors.New("transport")
}

func TestGroupCreateTwoStepNamedAndUnnamed(t *testing.T) {
	wanted := []string{"+15551230001"}
	weekend := "Weekend"
	for _, tc := range []struct {
		name       string
		groupName  *string
		answers    []func() (*gmproto.GetOrCreateConversationResponse, error)
		want       string
		wantCalls  int
		wantSecond *string
	}{
		{name: "named create", groupName: &weekend, answers: []func() (*gmproto.GetOrCreateConversationResponse, error){
			answerStatus(gmproto.GetOrCreateConversationResponse_CREATE_RCS, nil), answerStatus(gmproto.GetOrCreateConversationResponse_SUCCESS, createTestConversation()),
		}, want: `{"code":"created_rcs_group","google_conversation_id":"group-opaque"}`, wantCalls: 2, wantSecond: &weekend},
		{name: "unnamed create sends no name", groupName: nil, answers: []func() (*gmproto.GetOrCreateConversationResponse, error){
			answerStatus(gmproto.GetOrCreateConversationResponse_CREATE_RCS, nil), answerStatus(gmproto.GetOrCreateConversationResponse_SUCCESS, createTestConversation()),
		}, want: `{"code":"created_rcs_group","google_conversation_id":"group-opaque"}`, wantCalls: 2},
		{name: "existing group on lookup", groupName: nil, answers: []func() (*gmproto.GetOrCreateConversationResponse, error){
			answerStatus(gmproto.GetOrCreateConversationResponse_SUCCESS, createTestConversation()),
		}, want: `{"code":"existing_rcs_group","google_conversation_id":"group-opaque"}`, wantCalls: 1},
		{name: "unknown first RPC error", groupName: &weekend, answers: []func() (*gmproto.GetOrCreateConversationResponse, error){answerError},
			want: `{"code":"unknown"}`, wantCalls: 1},
		{name: "unknown first RPC nil response", groupName: &weekend, answers: []func() (*gmproto.GetOrCreateConversationResponse, error){
			func() (*gmproto.GetOrCreateConversationResponse, error) { return nil, nil },
		}, want: `{"code":"unknown"}`, wantCalls: 1},
		{name: "unknown second RPC error", groupName: &weekend, answers: []func() (*gmproto.GetOrCreateConversationResponse, error){
			answerStatus(gmproto.GetOrCreateConversationResponse_CREATE_RCS, nil), answerError,
		}, want: `{"code":"unknown"}`, wantCalls: 2, wantSecond: &weekend},
		{name: "unknown second RPC nil response", groupName: nil, answers: []func() (*gmproto.GetOrCreateConversationResponse, error){
			answerStatus(gmproto.GetOrCreateConversationResponse_CREATE_RCS, nil),
			func() (*gmproto.GetOrCreateConversationResponse, error) { return nil, nil },
		}, want: `{"code":"unknown"}`, wantCalls: 2},
		{name: "not rcs on lookup", groupName: nil, answers: []func() (*gmproto.GetOrCreateConversationResponse, error){
			answerStatus(gmproto.GetOrCreateConversationResponse_UNKNOWN, nil),
		}, want: `{"code":"not_rcs"}`, wantCalls: 1},
	} {
		call, calls := scriptedCreate(tc.answers...)
		got := runGroupCreate(context.Background(), call, wanted, tc.groupName)
		if got != tc.want || len(*calls) != tc.wantCalls {
			t.Fatalf("%s: result = %s calls = %d, want %s calls = %d", tc.name, got, len(*calls), tc.want, tc.wantCalls)
		}
		if tc.wantCalls == 2 {
			second := (*calls)[1]
			if !second.GetCreateRCSGroup() {
				t.Fatalf("%s: second request must set CreateRCSGroup", tc.name)
			}
			if tc.wantSecond == nil && second.RCSGroupName != nil {
				t.Fatalf("%s: unnamed request carried RCSGroupName %q", tc.name, second.GetRCSGroupName())
			}
			if tc.wantSecond != nil && second.GetRCSGroupName() != *tc.wantSecond {
				t.Fatalf("%s: RCSGroupName = %q, want %q", tc.name, second.GetRCSGroupName(), *tc.wantSecond)
			}
		}
	}
}

func TestGroupCreateClassificationRequiresMatchingRcsGroup(t *testing.T) {
	conversation := validGroupConversation()
	conversation.Participants[0].ID.Number = "+15551230001"
	conversation.Participants[1].ID.Number = "+15551230002"
	if result := groupCreateClassification(conversation, []string{"+15551230001"}, "created_rcs_group"); result != `{"code":"created_rcs_group","google_conversation_id":"group-opaque"}` {
		t.Fatalf("result = %s", result)
	}
	conversation.Type = gmproto.ConversationType_SMS
	if result := groupCreateClassification(conversation, []string{"+15551230001"}, "created_rcs_group"); result != `{"code":"not_rcs"}` {
		t.Fatalf("result = %s", result)
	}
}
