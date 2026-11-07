;;; piano-mode-test.el --- Tests for piano-mode  -*- lexical-binding: t; -*-

;; Run with:
;;   emacs --batch -Q -L . -l test/piano-mode-test.el \
;;         -f ert-run-tests-batch-and-exit

(require 'ert)
(require 'cl-lib)
(require 'piano-mode)

(defconst piano-test-dir
  (file-name-directory (or load-file-name buffer-file-name)))

(defconst piano-test-bin
  (expand-file-name "fixtures/bin" piano-test-dir))

(defconst piano-test-soundfont
  (expand-file-name "fixtures/dummy-soundfont" piano-test-dir))

(defun piano-test-wait-for (predicate &optional seconds)
  "Wait up to SECONDS (default 5) for PREDICATE to become non-nil.
Return PREDICATE's final value."
  (let ((deadline (+ (float-time) (or seconds 5))))
    (while (and (not (funcall predicate)) (< (float-time) deadline))
      (sit-for 0.05))
    (funcall predicate)))

(defun piano-test-slurp (file)
  "Return the contents of FILE."
  (with-temp-buffer
    (insert-file-contents file)
    (buffer-string)))

(defmacro piano-test-environment (&rest body)
  "Run BODY with fake helper/synthesizer programs and clean state."
  `(let ((piano-test-log (make-temp-file "piano-synth" nil ".log")))
     (unwind-protect
         (let ((exec-path (cons piano-test-bin exec-path))
               (piano-keys-program "piano-keys")
               (piano-synth-program "fluidsynth")
               (piano-soundfont piano-test-soundfont)
               (piano-input 'keys-helper)
               (piano-poll-ms 4)
               (process-environment
                (cons (format "PIANO_SYNTH_LOG=%s" piano-test-log)
                      process-environment)))
           (clrhash piano--active)
           (clrhash piano--timers)
           ,@body)
       (delete-file piano-test-log))))

(ert-deftest piano-test-helper-pipeline ()
  "A down/up pair from piano-keys becomes noteon/noteoff."
  (piano-test-environment
    (with-temp-buffer
      (piano-mode 1)
      (unwind-protect
          (progn
            (should (piano-test-wait-for
                     (lambda ()
                       (let ((log (piano-test-slurp piano-test-log)))
                         (and (string-match-p "noteon 0 60 100" log)
                              (string-match-p "noteoff 0 60" log))))))
            (should (= 0 (hash-table-count piano--active))))
        (piano-mode -1)))))

(ert-deftest piano-test-stop-kills-processes ()
  "Disabling the mode stops both helper processes."
  (piano-test-environment
    (with-temp-buffer
      (piano-mode 1)
      (unwind-protect
          (progn
            (should (process-live-p piano--keys-process))
            (should (process-live-p piano--synth-process)))
        (piano-mode -1))
      (should-not (process-live-p piano--keys-process))
      (should-not (process-live-p piano--synth-process)))))

(ert-deftest piano-test-lines-are-dispatched ()
  "down/up lines map to noteon/noteoff; unknown keys are ignored."
  (let ((calls '()))
    (clrhash piano--active)
    (cl-letf (((symbol-function 'piano--synth-send)
               (lambda (format-string &rest args)
                 (push (apply #'format format-string args) calls))))
      (piano--handle-keys-line "down A")
      (piano--handle-keys-line "up A")
      (piano--handle-keys-line "down Q")
      (piano--handle-keys-line "garbage"))
    (should (member "noteon 0 60 100\n" calls))
    (should (member "noteoff 0 60\n" calls))
    (should (= 0 (hash-table-count piano--active)))))

(ert-deftest piano-test-transpose ()
  "`piano-transpose' shifts the MIDI note."
  (let ((calls '())
        (piano-transpose 2))
    (clrhash piano--active)
    (cl-letf (((symbol-function 'piano--synth-send)
               (lambda (format-string &rest args)
                 (push (apply #'format format-string args) calls))))
      (piano--handle-keys-line "down A"))
    (should (member "noteon 0 62 100\n" calls))
    (should (member 62 (hash-table-keys piano--active)))))

(ert-deftest piano-test-emacs-keys-fallback ()
  "Without the helper, mapped keys play with a timer release."
  (let ((calls '())
        (piano-input 'emacs-keys)
        (piano-note-duration 0.2))
    (clrhash piano--active)
    (clrhash piano--timers)
    (cl-letf (((symbol-function 'piano--synth-send)
               (lambda (format-string &rest args)
                 (push (apply #'format format-string args) calls)))
              ((symbol-function 'piano--start-synth)
               (lambda ())))
      (with-temp-buffer
        (piano-mode 1)
        (unwind-protect
            (progn
              (let ((last-command-event ?a))
                (call-interactively #'piano-play-key))
              (should (member "noteon 0 60 100\n" calls))
              (sit-for 0.3)
              (should (member "noteoff 0 60\n" calls))
              (should (= 0 (hash-table-count piano--active))))
          (piano-mode -1))))))

(ert-deftest piano-test-keys-argument ()
  "The --keys argument carries every mapped key, upcased."
  (should (equal (piano--helper-keys-argument)
                 "A,W,S,E,D,F,T,G,Y,H,U,J,K,O,L,P,;")))

;;; piano-mode-test.el ends here
