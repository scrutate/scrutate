package smoke

func GoPoint() int {
	return 1
}

func useGo() int {
	return GoPoint() + 1
}
