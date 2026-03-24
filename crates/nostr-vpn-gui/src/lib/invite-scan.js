const RECOVERABLE_CAMERA_ERROR_NAMES = new Set([
  'ConstraintNotSatisfiedError',
  'DevicesNotFoundError',
  'NotFoundError',
  'OverconstrainedError',
])

export function buildInviteScanConstraintCandidates({ mobile }) {
  const hdVideo = {
    width: { ideal: 1280 },
    height: { ideal: 720 },
  }

  if (mobile) {
    return [
      {
        video: {
          ...hdVideo,
          facingMode: { ideal: 'environment' },
        },
        audio: false,
      },
      {
        video: {
          ...hdVideo,
          facingMode: 'environment',
        },
        audio: false,
      },
      { video: hdVideo, audio: false },
      { video: true, audio: false },
    ]
  }

  return [{ video: hdVideo, audio: false }, { video: true, audio: false }]
}

export function isRecoverableInviteScanError(err) {
  if (!err || typeof err !== 'object' || !('name' in err)) {
    return false
  }

  return RECOVERABLE_CAMERA_ERROR_NAMES.has(String(err.name))
}

export async function openInviteScanStream(getUserMedia, candidates) {
  let lastError = null

  for (const constraints of candidates) {
    try {
      return await getUserMedia(constraints)
    } catch (err) {
      lastError = err
      if (!isRecoverableInviteScanError(err)) {
        throw err
      }
    }
  }

  throw lastError ?? new Error('No camera constraints were attempted')
}
