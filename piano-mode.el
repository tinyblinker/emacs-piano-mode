;;; -*- lexical-binding: t -*-

;; map the keys to piano-play-key ()
(defvar piano-mode-map
  (let ((tmp-map (make-keymap)))
    (mapc (lambda (the-key) (keymap-set tmp-map the-key #'piano-play-key))
          '("1" "2" "3" "4" "5" "6" "7"))
    tmp-map)
  "keymap for 'piano-mode'")

;; (core) keys mapped here to be played
(defun piano-play-key ()
  "get input envent and send to 'piano-key-down-function' and 'piano-key-up-function'"
  (interactive)
  (let* ((key (char-to-string last-input-event)))
    (if (piano-key-is-down key) (piano-key-down key)
      (piano-key-up key))))

;; whether the piano's 'key' is down or not
;; (through external rust cli?)
(defun piano-key-is-down (key))

;; the pianos's 'key' is down
;; (play the key)
(defun piano-key-down (key)
  (interactive)
  (message "key-down:%s" key))

;; the piano's 'key' is up
(defun piano-key-up (key)
  (interactive)
  (message "key-up:%s" key))

;;;###autoload
(define-minor-mode piano-mode
  "Toggle piano mode"
  :global nil
  :lighter " Piano"
  :keymap piano-mode-map)

;;; Piano-mode.el ends here
