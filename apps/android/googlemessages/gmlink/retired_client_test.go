package gmlink

import "testing"

func TestCloseRetiresClientBeforeNextOpen(t *testing.T) {
	first, err := Open(nil, &testHost{})
	if err != nil {
		t.Fatal(err)
	}
	first.Close()

	second, err := Open(nil, &testHost{})
	if err != nil {
		t.Fatalf("retired client held process slot: %v", err)
	}
	second.Close()
}
