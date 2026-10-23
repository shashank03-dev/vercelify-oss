import { LazyMotion, MotionConfig, domAnimation } from "framer-motion";
import { BeamTrack } from "./components/Beam";
import { Compare, Faq, FinalCta, Footer, Horizon } from "./components/Closing";
import { Features } from "./components/Features";
import { FromVercel } from "./components/FromVercel";
import { Hero, Nav } from "./components/Hero";
import { Stack, Statement } from "./components/Intro";
import { Hardware, Network } from "./components/Machine";
import { Steps } from "./components/Steps";

export default function App() {
  // Only the DOM animation features ship (m.* components, not motion.*).
  return (
    <LazyMotion features={domAnimation} strict>
      {/* Reduced motion: framer-motion keeps fades but drops the slides and springs. */}
      <MotionConfig reducedMotion="user">
        <a
          href="#content"
          className="sr-only fixed top-3 left-3 z-[60] rounded-lg bg-fg px-4 py-2 text-[14px] font-medium text-bg focus:not-sr-only"
        >
          Skip to content
        </a>
        <Nav />
        <main id="content" tabIndex={-1} className="outline-none">
          <Hero />
          {/* The beam leaves the hero's app window and runs down to the horizon. */}
          <BeamTrack>
            <Stack />
            <Statement />
            <Steps />
            <Features />
            <FromVercel />
            <Hardware />
            <Network />
            <Compare />
            <Faq />
            <FinalCta />
            <Footer />
          </BeamTrack>
          <Horizon />
        </main>
      </MotionConfig>
    </LazyMotion>
  );
}
