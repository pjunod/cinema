import SwiftUI

/// An explicit server offer, resolved to a currently playable lineup entry.
/// The refusal's attachment generation revokes buttons left open by an older
/// request. A guide number alone is never permission to guess a channel ID.
struct LiveTvCapacityOffer: Identifiable, Equatable {
    let channel: LiveTvChannel
    let generation: Int
    var id: String { channel.id }

    static func resolve(
        _ failure: LiveTvFailure, lineup: [LiveTvChannel], generation: Int
    ) -> [Self] {
        guard failure.code == "tuner_capacity" else { return [] }
        var seen = Set<String>()
        return failure.watchable.compactMap { holder in
            guard let channel = lineup.first(where: { $0.id == holder.channelId }),
                  channel.watchable, seen.insert(channel.id).inserted else { return nil }
            return Self(channel: channel, generation: generation)
        }
    }
}

struct LiveTvCapacityOfferActions: View {
    @ObservedObject var live: LiveTvPlayerController
    var body: some View {
        ScrollView(.horizontal) {
            HStack(spacing: 12) {
                ForEach(live.capacityOffers) { offer in
                    Button("Watch \(offer.channel.guideNumber) instead") {
                        Task { await live.watchOffer(offer) }
                    }
                    .accessibilityIdentifier("live-tv-watch-offer-\(offer.channel.guideNumber)")
                    .accessibilityHint("Watch an available shared channel without stopping another viewer or recording")
                    #if os(tvOS)
                    .buttonStyle(TVReadableButtonStyle(prominent: false))
                    .focusEffectDisabled()
                    #else
                    .buttonStyle(.bordered)
                    #endif
                }
            }
            .padding(.horizontal, 12).padding(.vertical, 6)
        }
        .fixedSize(horizontal: false, vertical: true)
    }
}
