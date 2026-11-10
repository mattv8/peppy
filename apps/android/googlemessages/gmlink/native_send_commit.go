package gmlink

import (
	"context"
	"errors"
	"reflect"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
	"google.golang.org/protobuf/proto"
)

func nativeCommitMatchesTicket(commit nativeRcsCommitProjection, ticket *nativeRcsTicket) bool {
	if ticket.facts == nil || ticket.targetObservation == nil || ticket.conversation == nil {
		return false
	}
	facts, observation := ticket.facts, ticket.targetObservation
	mapping := facts.StoredMapping
	expected := nativeRcsTargetSnapshot{
		ProofVersion: mapping.ProofVersion, PeppyConversationID: mapping.ConversationID, GoogleConversationID: observation.GoogleConversationID,
		ConversationType: observation.ConversationType, Status: observation.Status, SendMode: observation.SendMode,
		ReadOnly: observation.ReadOnly, DefaultOutgoing: observation.DefaultOutgoing, Participants: observation.Participants,
		Class: mapping.Class, CanonicalRecipient: mapping.Proof.CanonicalParticipant, CanonicalOwnIdentity: mapping.Proof.OwnIdentity,
		IdentityRegion: observation.IdentityRegion, CanonicalizerRevision: observation.CanonicalizerRevision,
		SelectedSIMID: observation.SelectedSIMID, CarrierBinding: observation.CarrierBinding, Outgoing: observation.Outgoing, Evidence: observation.NativeEvidence,
	}
	// Body and tmp ID are supplied only by the captured core-issued native handle.
	return commit.CommandID == ticket.request.CommandID && commit.MappingRevision == mapping.MappingRevision && commit.ExpectedMemberDigest == ticket.request.ExpectedMemberDigest &&
		commit.ProofVersion == mapping.ProofVersion && commit.ProofDigest == mapping.Proof.Digest && reflect.DeepEqual(commit.Target, expected)
}

// CommitNativeRcs consumes a fully bound ticket before the single transport admission.
func (link *Link) CommitNativeRcs(ticketID, commitJSON string) string {
	commit, valid := decodeNativeRcsCommit(commitJSON)
	link.mu.Lock()
	ticket := link.nativeTicket
	if !valid || !link.nativeTicketCurrentLocked(ticket, ticketID) || !nativeCommitMatchesTicket(commit, ticket) ||
		link.nativeInFlight != nil || link.runCtx == nil || link.client == nil {
		result := link.rejectNativeTargetLocked(ticket, ticketID)
		link.mu.Unlock()
		return result
	}
	remaining := ticket.expiresAt.Sub(link.nowForTicketLocked())
	if remaining > rcsSendDeadline {
		remaining = rcsSendDeadline
	}
	operationCtx, cancel := context.WithTimeout(link.runCtx, remaining)
	flight := &nativeRcsInFlight{ticketID: ticketID}
	link.nativeTicket = nil
	link.nativeInFlight = flight
	client, effect := link.client, link.sendMessage
	link.mu.Unlock()
	defer cancel()
	defer func() {
		link.mu.Lock()
		if link.nativeInFlight == flight {
			link.nativeInFlight = nil
		}
		link.mu.Unlock()
	}()
	reject := func() string { return nativeRcsJSON(nativeRcsPrepareResult{Code: "rejected"}) }
	if !link.lockRPC(operationCtx) {
		return reject()
	}
	defer link.rpcMu.Unlock()
	link.mu.Lock()
	current := link.nativeFlightCurrentLocked(ticket, flight) && operationCtx.Err() == nil
	link.mu.Unlock()
	if !current {
		return reject()
	}
	fetched, code := link.getConversation(operationCtx, client, ticket.targetObservation.GoogleConversationID)
	if code != "" || operationCtx.Err() != nil {
		return reject()
	}
	if ticket.group && groupMemberDigest(fetched) != ticket.request.ExpectedMemberDigest {
		return nativeRcsJSON(nativeRcsPrepareResult{Code: "members_changed"})
	}
	if !proto.Equal(fetched, ticket.conversation) {
		return reject()
	}
	link.mu.Lock()
	current = link.nativeFlightCurrentLocked(ticket, flight)
	link.mu.Unlock()
	if !current {
		return reject()
	}
	payload, code := validatedPayload(ticket.conversation, ticket.settings, rcsSendRequest{
		TmpID: commit.TmpID, GoogleConversationID: ticket.targetObservation.GoogleConversationID, Content: commit.MessageBody,
	}, ticket.group)
	if code != "" {
		return reject()
	}
	if effect == nil {
		effect = client.SendMessage
	}
	ctx := context.WithValue(operationCtx, sendAdmissionContextKey{}, sendAdmission(func() bool {
		return link.admitNativeSend(operationCtx, ticket, flight)
	}))
	response, err := effect(ctx, payload)
	link.mu.Lock()
	admitted := flight.admitted
	membersChanged := flight.membersChanged
	link.mu.Unlock()
	if membersChanged {
		return nativeRcsJSON(nativeRcsPrepareResult{Code: "members_changed"})
	}
	if errors.Is(err, errSendAdmissionRevoked) && !admitted {
		return reject()
	}
	if err == nil && admitted && response != nil && response.GetGoogleAccountSwitch() == nil && response.GetStatus() == gmproto.SendMessageResponse_SUCCESS {
		return nativeRcsJSON(nativeRcsPrepareResult{Code: "accepted"})
	}
	return nativeRcsJSON(nativeRcsPrepareResult{Code: "outcome_unknown"})
}

func (link *Link) nativeFlightCurrentLocked(ticket *nativeRcsTicket, flight *nativeRcsInFlight) bool {
	return link.nativeInFlight == flight && flight.ticketID == ticket.id && !flight.revoked && !flight.admitted &&
		link.nativeFrozenCurrentLocked(ticket) && proto.Equal(ticket.conversation,
		link.currentSupportedRcsConversationLocked(ticket.targetObservation.GoogleConversationID, ticket.generation, ticket.streamEpoch))
}

func (link *Link) admitNativeSend(ctx context.Context, ticket *nativeRcsTicket, flight *nativeRcsInFlight) bool {
	link.mu.Lock()
	hook := link.beforeSendAdmission
	link.mu.Unlock()
	if hook != nil {
		hook()
	}
	link.mu.Lock()
	defer link.mu.Unlock()
	if ticket.group && groupMemberDigest(link.conversationRecords[ticket.targetObservation.GoogleConversationID]) != ticket.request.ExpectedMemberDigest {
		flight.membersChanged = true
		return false
	}
	if ctx.Err() != nil || !link.nativeFlightCurrentLocked(ticket, flight) {
		return false
	}
	flight.admitted = true
	return true
}
