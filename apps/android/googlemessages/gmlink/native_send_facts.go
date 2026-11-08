package gmlink

// BindNativeRcsFacts binds stored core authority to this exact native observation once.
func (link *Link) BindNativeRcsFacts(ticketID, factsJSON string) string {
	facts, valid := decodeNativeRcsFacts(factsJSON)
	link.mu.Lock()
	defer link.mu.Unlock()
	ticket := link.nativeTicket
	if !valid || !link.nativeTicketCurrentLocked(ticket, ticketID) || ticket.targetObservation == nil || ticket.facts != nil {
		return link.rejectNativeTargetLocked(ticket, ticketID)
	}
	observation := ticket.targetObservation
	request, mapping := facts.Request, facts.StoredMapping
	if request.CommandID != ticket.request.CommandID || !validNativeUUID(request.PeppyConversationID) ||
		request.PeppyConversationID != mapping.ConversationID || request.CanonicalRecipient != mapping.Proof.CanonicalParticipant ||
		mapping.GoogleConversationID != observation.GoogleConversationID || mapping.MappingRevision != observation.MappingRevision ||
		mapping.VerifiedAtMS != observation.ObservedAtMS || mapping.Proof.IdentityRegion != observation.IdentityRegion ||
		mapping.ProofVersion != 2 || mapping.Class != "OneToOne" {
		return link.rejectNativeTargetLocked(ticket, ticketID)
	}
	ticket.facts = &facts
	return nativeRcsJSON(nativeRcsPrepareResult{Code: "facts_bound"})
}
