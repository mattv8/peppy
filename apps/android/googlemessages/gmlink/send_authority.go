package gmlink

import (
	"cmp"
	"slices"

	"go.mau.fi/mautrix-gmessages/pkg/libgm/gmproto"
)

// sendAuthorityParticipant holds the participant facts that classification, the mapping
// observation, and the member digest read.
type sendAuthorityParticipant struct {
	participantID   string
	number          string
	formattedNumber string
	identifierType  gmproto.IdentifierType
	self            bool
	visible         bool
}

// sameSendAuthority reports whether two records of one conversation agree on every fact that
// selects, classifies, or routes a send. The latest message, read state, timestamps, names, and
// avatars change with ordinary activity and differ between Google's responses, so they are not
// compared.
func sameSendAuthority(left, right *gmproto.Conversation) bool {
	if left == nil || right == nil {
		return false
	}
	return left.GetConversationID() == right.GetConversationID() &&
		left.GetType() == right.GetType() &&
		left.GetStatus() == right.GetStatus() &&
		left.GetSendMode() == right.GetSendMode() &&
		left.GetReadOnly() == right.GetReadOnly() &&
		left.GetIsGroupChat() == right.GetIsGroupChat() &&
		left.GetDefaultOutgoingID() == right.GetDefaultOutgoingID() &&
		slices.Equal(sendAuthorityParticipants(left), sendAuthorityParticipants(right))
}

// sendAuthorityParticipants returns the participants in an order independent of the response.
func sendAuthorityParticipants(conversation *gmproto.Conversation) []sendAuthorityParticipant {
	participants := make([]sendAuthorityParticipant, 0, len(conversation.GetParticipants()))
	for _, participant := range conversation.GetParticipants() {
		participants = append(participants, sendAuthorityParticipant{
			participantID:   participant.GetID().GetParticipantID(),
			number:          participant.GetID().GetNumber(),
			formattedNumber: participant.GetFormattedNumber(),
			identifierType:  participant.GetID().GetType(),
			self:            participant.GetIsMe(),
			visible:         participant.GetIsVisible(),
		})
	}
	slices.SortFunc(participants, func(left, right sendAuthorityParticipant) int {
		return cmp.Or(
			cmp.Compare(left.participantID, right.participantID),
			cmp.Compare(left.number, right.number),
			cmp.Compare(left.formattedNumber, right.formattedNumber),
			cmp.Compare(left.identifierType, right.identifierType),
			compareBool(left.self, right.self),
			compareBool(left.visible, right.visible),
		)
	})
	return participants
}

func compareBool(left, right bool) int {
	switch {
	case left == right:
		return 0
	case right:
		return -1
	default:
		return 1
	}
}
