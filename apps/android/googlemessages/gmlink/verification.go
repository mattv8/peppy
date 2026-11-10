package gmlink

const (
	verificationCapabilityReceive = "receive"
	verificationCapabilityIndex   = "index"
	verificationCapabilitySend    = "send"
	verificationCapabilityTyping  = "typing"
	verificationCapabilityGroups  = "groups"
)

// ownerVerification is runtime-only stream authority.
type ownerVerification struct {
	Capabilities []string
	generation   uint64
}

func (link *Link) hasVerificationCapabilityLocked(capability string) bool {
	if link.closed || link.ownerVerification == nil || link.ownerVerification.generation != link.generation {
		return false
	}
	for _, granted := range link.ownerVerification.Capabilities {
		if granted == capability {
			return true
		}
	}
	return false
}

func (link *Link) hasReceiveVerificationLocked() bool {
	return link.hasVerificationCapabilityLocked(verificationCapabilityReceive)
}

func (link *Link) hasIndexVerificationLocked() bool {
	return link.hasVerificationCapabilityLocked(verificationCapabilityIndex)
}

func (link *Link) hasSendVerificationLocked() bool {
	return link.hasVerificationCapabilityLocked(verificationCapabilitySend)
}

func (link *Link) hasTypingOutboundVerificationLocked() bool {
	return link.hasVerificationCapabilityLocked(verificationCapabilityTyping)
}

func (link *Link) hasGroupsVerificationLocked() bool {
	// Groups stay unminted: group chats are not built.
	return link.hasVerificationCapabilityLocked(verificationCapabilityGroups)
}

func (link *Link) mintRuntimeVerificationLocked() {
	if link.closed {
		link.ownerVerification = nil
		return
	}
	if link.state != stateLinked || !link.running || !link.streamOpen || link.runGeneration != link.generation {
		return
	}
	link.ownerVerification = &ownerVerification{
		Capabilities: []string{verificationCapabilityReceive, verificationCapabilityIndex, verificationCapabilitySend, verificationCapabilityTyping},
		generation:   link.generation,
	}
}
