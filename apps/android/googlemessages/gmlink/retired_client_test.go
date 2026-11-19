package gmlink

import "testing"

func TestCloseRetiresClientBeforeNextOpen(t *testing.T) {
	first := openTestLink(t, nil, &testHost{})
	first.Close()

	second, err := Open(nil, &testHost{})
	if err != nil {
		t.Fatalf("retired client held process slot: %v", err)
	}
	second.Close()
	awaitSafety(t, "retired client output drain", func() bool { return openLink.Load() != second })
}
