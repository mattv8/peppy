package gmlink

import (
	"context"
	"time"

	"github.com/google/uuid"
	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

// BindNativeRcsTarget obtains fresh read-only evidence without extending the prepared ticket.
func (link *Link) BindNativeRcsTarget(ticketID, googleConversationID, token string) string {
	link.mu.Lock()
	ticket := link.nativeTicket
	if !link.nativeTicketCurrentLocked(ticket, ticketID) || ticket.routeObservation == nil || ticket.targetObservation != nil ||
		token != snapshotToken(ticket.generation, ticket.streamEpoch, ticket.indexRevision) || link.runCtx == nil || link.client == nil {
		result := link.rejectNativeTargetLocked(ticket, ticketID)
		link.mu.Unlock()
		return result
	}
	conversation := link.currentSupportedRcsConversationLocked(googleConversationID, ticket.generation, ticket.streamEpoch)
	if conversation == nil || conversation.GetReadOnly() || conversation.GetSendMode() != gmproto.ConversationSendMode_SEND_MODE_AUTO ||
		conversation.GetDefaultOutgoingID() != ticket.selectedCard.GetSIMParticipant().GetID() {
		result := link.rejectNativeTargetLocked(ticket, ticketID)
		link.mu.Unlock()
		return result
	}
	group := conversation.GetIsGroupChat()
	if group && (!link.hasGroupsVerificationLocked() || ticket.request.ExpectedMemberDigest == "") {
		result := link.rejectNativeTargetLocked(ticket, ticketID)
		link.mu.Unlock()
		return result
	}
	if !group && ticket.request.ExpectedMemberDigest != "" {
		result := link.rejectNativeTargetLocked(ticket, ticketID)
		link.mu.Unlock()
		return result
	}
	remaining := ticket.expiresAt.Sub(link.nowForTicketLocked())
	if remaining > rcsSendDeadline {
		remaining = rcsSendDeadline
	}
	operationCtx, cancel := context.WithTimeout(link.runCtx, remaining)
	client := link.client
	clock := link.ticketNow
	if clock == nil {
		clock = time.Now
	}
	link.mu.Unlock()
	defer cancel()

	if !link.lockRPC(operationCtx) {
		link.mu.Lock()
		defer link.mu.Unlock()
		return link.rejectNativeTargetLocked(ticket, ticketID)
	}
	defer link.rpcMu.Unlock()
	link.mu.Lock()
	if !link.nativeTicketCurrentLocked(ticket, ticketID) || ticket.targetObservation != nil {
		result := link.rejectNativeTargetLocked(ticket, ticketID)
		link.mu.Unlock()
		return result
	}
	link.mu.Unlock()
	fetched, code := link.getConversation(operationCtx, client, googleConversationID)
	receivedAtMS := clock().UnixMilli()
	link.mu.Lock()
	defer link.mu.Unlock()
	if code != "" || operationCtx.Err() != nil || !link.nativeTicketCurrentLocked(ticket, ticketID) ||
		ticket.targetObservation != nil || !sameSendAuthority(conversation, fetched) ||
		!sameSendAuthority(conversation, link.currentSupportedRcsConversationLocked(googleConversationID, ticket.generation, ticket.streamEpoch)) {
		return link.rejectNativeTargetLocked(ticket, ticketID)
	}
	observation := *ticket.routeObservation
	observation.GoogleConversationID = googleConversationID
	observation.GoogleCandidateCount = 1
	observation.ConversationType = "Rcs"
	observation.Status = "eligible"
	observation.SendMode = "Automatic"
	observation.ReadOnly = conversation.GetReadOnly()
	observation.DefaultOutgoing = conversation.GetDefaultOutgoingID() == ticket.selectedCard.GetSIMParticipant().GetID()
	observation.Participants = make([]nativeRcsParticipant, 0, len(conversation.GetParticipants()))
	for _, participant := range conversation.GetParticipants() {
		id := participant.GetID()
		observation.Participants = append(observation.Participants, nativeRcsParticipant{
			Identity: id.GetNumber(), GoogleParticipantID: id.GetParticipantID(), IdentifierType: int32(id.GetType()),
			IsVisible: participant.GetIsVisible(), IsMe: participant.GetIsMe(),
		})
	}
	observation.NativeEvidence.LookupID = uuid.NewString()
	observation.NativeEvidence.LookupComplete = true
	observation.NativeEvidence.GoogleCandidateCount = 1
	observation.NativeEvidence.ConversationObservedAtMS = receivedAtMS
	observation.NativeEvidence.SettingsValidatedAtMS = link.nowForTicketLocked().UnixMilli()
	encoded, inner, valid := encodeNativeObservation("target_bound", observation)
	if !valid {
		return link.rejectNativeTargetLocked(ticket, ticketID)
	}
	ticket.targetObservation = &observation
	ticket.targetObservationJSON = inner
	ticket.conversation = conversation
	ticket.group = group
	return encoded
}

// A stale lookup may finish after cancellation and a successor preparation.
func (link *Link) rejectNativeTargetLocked(ticket *nativeRcsTicket, ticketID string) string {
	if ticket != nil && ticket.id == ticketID && link.nativeTicket == ticket {
		link.nativeTicket = nil
	}
	return nativeRcsJSON(nativeRcsPrepareResult{Code: "rejected"})
}
