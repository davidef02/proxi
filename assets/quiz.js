/* quiz.js — widget di retrieval practice riusabile da ogni lezione.
 *
 * Markup atteso:
 *   <div class="quiz">
 *     <div class="quiz-question">
 *       <p>Domanda?</p>
 *       <div class="quiz-options">
 *         <button class="quiz-option" data-correct="true"
 *                 data-feedback="Perché è giusta.">Risposta</button>
 *         <button class="quiz-option" data-feedback="Perché è sbagliata.">Altra</button>
 *       </div>
 *       <div class="quiz-feedback"></div>
 *     </div>
 *     <p class="quiz-score"></p>
 *   </div>
 *
 * Feedback immediato al primo click, una sola risposta per domanda (niente
 * ritentativi: il costo dell'errore è ciò che costruisce storage strength).
 */

const shuffle = (nodes) => {
  const arr = [...nodes];
  for (let i = arr.length - 1; i > 0; i--) {
    const j = Math.floor(Math.random() * (i + 1));
    [arr[i], arr[j]] = [arr[j], arr[i]];
  }
  return arr;
};

const updateScore = (quiz) => {
  const score = quiz.querySelector(".quiz-score");
  if (!score) {
    return;
  }
  const questions = quiz.querySelectorAll(".quiz-question");
  const answered = quiz.querySelectorAll('.quiz-question[data-answered="true"]');
  const right = quiz.querySelectorAll('.quiz-question[data-result="right"]');
  score.textContent =
    answered.length === 0
      ? `${questions.length} domande — rispondi a memoria, senza rileggere.`
      : `${right.length}/${answered.length} corrette (${answered.length} di ${questions.length} fatte)`;
};

const answer = (question, picked) => {
  question.dataset.answered = "true";
  const isRight = picked.dataset.correct === "true";
  question.dataset.result = isRight ? "right" : "wrong";

  for (const option of question.querySelectorAll(".quiz-option")) {
    option.disabled = true;
    option.classList.add(
      option.dataset.correct === "true" ? "is-correct" : "is-wrong"
    );
  }
  picked.classList.add("is-picked");

  const feedback = question.querySelector(".quiz-feedback");
  if (feedback) {
    feedback.textContent = picked.dataset.feedback ?? "";
    feedback.classList.add("is-visible", isRight ? "ok" : "ko");
  }
};

// L'ordine delle opzioni non deve diventare un indizio memorizzabile.
for (const options of document.querySelectorAll(".quiz-options")) {
  for (const option of shuffle(options.children)) {
    options.append(option);
  }
}

for (const quiz of document.querySelectorAll(".quiz")) {
  updateScore(quiz);
  quiz.addEventListener("click", (event) => {
    const picked = event.target.closest(".quiz-option");
    if (!picked) {
      return;
    }
    const question = picked.closest(".quiz-question");
    if (!question || question.dataset.answered === "true") {
      return;
    }
    answer(question, picked);
    updateScore(quiz);
  });
}
