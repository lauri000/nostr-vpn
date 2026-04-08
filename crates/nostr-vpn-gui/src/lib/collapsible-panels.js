export const reconcileAutoOpenPanelState = (currentOpen, previousIssueCount, nextIssueCount) => {
  if (previousIssueCount == null) {
    return nextIssueCount > 0
  }

  if (previousIssueCount === 0 && nextIssueCount > 0) {
    return true
  }

  return currentOpen
}
